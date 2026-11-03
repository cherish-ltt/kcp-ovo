# v0.5.2 超时退避验证

## 范围与方法

- 修复 #38，基线为 v0.5.1 的 `48b16cd135a009eaef4371521b0a70bafb55fb36`。
- Rust/Cargo 1.99.1、macOS arm64；复用 `tests/retransmit.rs`、`Sink`、`Sim`，不访问公网或修改系统网络。
- 修改共享超时路径，不改公共 API、协议线格式、RTT 估计、快速重传或发送成功后提交状态的规则。
- 以下时间均为虚拟毫秒，不是实际耗时或生产性能。

## 最小复现与精确期限

```bash
cargo test --locked --test retransmit --all-features -- --nocapture
```

首传时刻为 0，初始 RTO 为 200，不回 ACK，按到期时间推进。修复前四项新增回归失败：

| 条件 | 修复前实际值 | 修复后实际值 |
| --- | --- | --- |
| normal 首次超时后的 `(rto, resendts)` | `(200, 450)` | `(400, 625)` |
| fast 首次超时后的 `(rto, resendts)` | `(400, 400)` | `(300, 500)` |
| normal、连接估计 RTO 已增至 1000 | `(1000, 550)` | `(1200, 1425)` |
| 超时与 fastack 同时满足、normal | 一次输出，但 `(rto, resendts)=(200, 450)` | 一次输出，`(400, 625)`，fastack 清零 |

修复后首传及五次连续超时的序列：

| 模式 | RTO 序列 | 下一重传期限序列 |
| --- | --- | --- |
| normal | 200, 400, 800, 1600, 3200, 6400 | 225, 625, 1425, 3025, 6225, 12625 |
| fast | 200, 300, 450, 675, 1012, 1518 | 200, 500, 950, 1625, 2637, 4155 |

同一回归还覆盖 u32/u64 时钟回绕、到期前不重传、`check` 返回可执行期限、
60 秒限幅、饱和累加、dead_link 精确停止、快速重传不退避、重传 ACK 不污染 RTT，
以及恢复后新段可正常采样。边界测试通过公开字段设置极端 RTO 与 fastack；
真实 ACK 驱动快速重传由既有 `tests/timing.rs` 回归覆盖。

## 同条件传输对照

每格发送 1 MiB，固定 seed `0x38`，单程延迟 10 ms；`interval=10`、
`fastresend=2`、`nocwnd=true`、流模式及窗口保持一致，只切换 `nodelay`。
因此这不是 `normal_mode()` 与 `fast_mode()` 两套预设的整体性能对比。
成簇丢包使用 `with_burst(0.03, 0.5)`，黑洞使用 `with_blackout(0, 3000)`。
每格最多 120000 tick，完成要求为逐字节一致且发送队列确认排空。

| nodelay | 链路 | 修复前 tick / 输出包 | 修复后 tick / 输出包 |
| --- | --- | ---: | ---: |
| false | 正常 | 971 / 1581 | 971 / 1581 |
| false | 成簇丢包 | 1441 / 1627 | 1441 / 1627 |
| false | 3 秒黑洞 | 4141 / 1571 | 4011 / 1561 |
| true | 正常 | 971 / 1581 | 971 / 1581 |
| true | 成簇丢包 | 1371 / 2155 | 1451 / 2178 |
| true | 3 秒黑洞 | 4191 / 1562 | 5141 / 1563 |

修复前后十二格均完整交付。输出包统计含双向数据及控制包，不等于重传次数。
固定 seed 不保证不同发送时序经历同一丢包轨迹；尤其 fast 的本组恢复更慢，不能将
修正退避语义宣传为无条件性能提升。正常链路观测未变也不能证明所有负载均无回归。

## 限制

未测试公网、Windows、长时间稳定性、带宽竞争公平性或下游 lynn_tcp 集成。
本记录的仿真结果不能替代真实弱网或生产 SLO 验收。

## 本地验证

- 定向 `retransmit`：11 passed；将最终测试复制到未修复基线的隔离副本后，7 passed、4 failed，失败值与上表一致。
- `cargo test --workspace --all-features`：336 passed、0 failed；59 个单元测试、274 个集成测试、3 个文档测试，另有 5 个既有 ignored 文档测试。
- 测试日志的套件执行时间合计 5.10 秒，其中集成套件 4.05 秒，不含编译时间，满足 30 秒黑盒预算。
- 格式、全目标 Clippy、check、build 通过；`cargo test --locked --no-default-features --test api_contract` 为 24 passed。
- `cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 80` 返回 0，总行覆盖率 95.47%（1937/2029），协议核心 97.05%；未排除生产文件。
- 覆盖率报告带 `18 functions have mismatched data` 警告，门槛检查通过但不能声称获得无警告覆盖率证明；分支覆盖率未测。
- `prek run --all-files` 返回 0，适用钩子全部通过，图片大小与 JSON 无匹配文件而跳过；既有 Node hook 缓存版本提示未阻止 ast-grep 实际通过。

首次复用构建目录的隔离负对照意外通过，与旧源码不符，该次结果作废；随后只指定
Cargo 二进制仍因依赖目录工具链选择而出现 `E0514` 编译器不兼容，亦不计入验证。
最终负对照明确固定 Rust/Cargo 1.99.1、独立输出目录并禁用编译包装器，实际执行后
得到四项预期失败。正式全量检查同样绕过包装器，使用独立的项目构建目录。
稳定版 rustfmt 对既有两个 nightly 配置项有提示，未修改格式规则或压制检查。

版本更新后的 `cargo check --offline --workspace --all-features` 通过，锁文件仅改变本包版本。
`cargo doc --locked --no-deps --all-features` 生成成功，但既有 `src/stream.rs` 文档中的
`Vec<u8>` 未加代码标记，触发 `unclosed HTML tag` 警告；本次未修改该接口。
首次发布预演因本机 crates.io 源替换要求显式指定 registry 而退出，随后
`cargo publish --dry-run --locked --registry crates-io` 完成打包和编译校验，没有实际上传。
它提示锁定的 `chacha20 0.10.0` 已被撤回；`cargo tree --edges normal -i chacha20`
无依赖路径，反向完整树仅经 proptest/rand 开发依赖进入。本次未将其认定为可利用漏洞，
也未扩展升级依赖。
