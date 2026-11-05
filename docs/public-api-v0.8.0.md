# v0.8.0 公开 API 验证记录

关联 [Issue #46](https://github.com/cherish-ltt/kcp-ovo/issues/46)。基线为 v0.7.0 的
`c256baf9f3a6e613177e2f08400e7bc8594d80af`，验证日期为 2026-11-05。
本地环境：macOS 27.2、aarch64-apple-darwin、Rust 1.99.1、LLVM 23.1.1。

## 可观察差异

| 相同操作 | v0.7.0 | v0.8.0 |
| --- | --- | --- |
| 独立下游将 `recv` 和输出回调声明为 `Vec<u8>` | 编译失败，报告 E0308 / E0631，实际要求 `Bytes` | 编译通过；独立核心项目只直接依赖本库并完成消息/流模式收发 |
| 下游精确选择 Tokio 1.54.1、bytes 1.12.1 | Cargo 因本库精确锁定 bytes 1.11.1 而解析失败 | 统一解析到下游指定版本，4096 字节真实回环收发及 try 接口通过 |
| 解析包后复用输入缓冲区 | `Bytes` 共享原分配 | Header 借用切片，Packet 持有独立 `Vec<u8>`，覆盖原输入后载荷不变 |
| 接收队列中有多个独立单段消息，逐条 `recv` | 旧分支在队列长度大于 1 时拼接复制 | 每条消息直接移动其载荷，返回指针与原分配相同 |
| 下游自定义标准库 `ToSocketAddrs` | 高层要求 Tokio trait，不能直接使用仅实现标准 trait 的类型 | 高层接收标准 trait，在后台解析并完成收发 |

仅删除重导出不会消除精确依赖约束造成的解析冲突；本次同时处理公开类型和 Cargo 约束。
兼容范围含义依据 [Cargo resolver 文档](https://doc.rust-lang.org/cargo/reference/resolver.html#version-requirements)。

## 验证范围

- 全 feature 测试共 375 项通过、0 失败，包含 371 项单元/集成测试和 4 项文档测试；另有 5 项既有文档示例标记 ignored。
- `cargo fmt --all -- --check`、全目标 Clippy（`-D warnings`）、workspace check/build/test 和 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` 通过。
- `cargo test --locked --no-default-features --test api_contract` 的 32 项通过，保留显式 System 全局分配器兼容性。
- 独立 `tests/downstream/core/` 项目关闭默认 feature，只直接依赖本库；通过标准库公开类型、包/段所有权及两种核心模式的 4096 字节传输验证，不使用异步运行时。
- 独立 `tests/downstream/stream/` 项目选择 Tokio 1.54.1 / bytes 1.12.1，验证标准地址、自定义解析、监听器多地址回退及真实 UDP 收发；两个独立项目各 2 项测试及 fmt/Clippy 均通过，由 CI 和 Release 单独执行。单线程运行时测试确认解析发生在其他线程，空地址、解析错误与 panic 均显式返回相应错误。
- `queued_single_segment_messages_move_each_payload` 负向验证：临时恢复旧分支条件后指针断言失败（退出码 101），恢复修改后通过。Packet/Builder 的移动、短头/短载荷输入推进及借用生命周期也有断言。
- 全量黑盒保留字节精确、消息边界、丢包/乱序/重复/黑洞、恶意输入、序号回绕、大数据与并发场景；未修改协议策略或放宽已有断言。
- `prek run --all-files` 完整通过，包含密钥扫描、ast-grep 和 Rust fmt/check/Clippy/全目标测试；图片与 JSON 检查因无匹配文件跳过。工具提示跳过不匹配的 Node 缓存环境，最终 ast-grep 检查已执行并通过。
- `cargo publish --dry-run --allow-dirty --registry crates-io` 完成打包及编译验证；本机默认镜像配置要求显式选择 crates.io。锁文件中的既有间接依赖 `chacha20 0.10.0` 提示已撤回，未在本次 API 迁移中更新。dry-run 不代表已实际发布。

## 覆盖率

`cargo llvm-cov --workspace --all-features --summary-only` 返回 0，总行覆盖率为 **95.00%**
（2240 行，112 行未覆盖），区域覆盖率 95.46%，函数覆盖率 92.73%，超过 80% 门槛。
报告伴随 `220 functions have mismatched data` 警告，不能称为无警告的覆盖率证明。

对本轮 24 个测试二进制和现存 profdata 使用同工具链 `llvm-cov report -dump` 核对：
220 条警告全部是零 hash 映射，去重后涉及 13 个符号；13 个符号均在 profdata 中有非零
hash 和正执行计数，JSON export 也保留相同计数。正序/倒序读取二进制时逐文件、逐函数
export 完全一致，源文件报告均为 2128/2240 行。未发现这些真实符号计数丢失，仍不扩大为
对所有行映射绝对无影响的证明。未隐藏警告、过滤生产文件或放宽门槛。

## 同机性能观测

修改前保存 Criterion `before-std-api` 基线，修改后对同一基线测量；每项 20 个样本，
固定随机种子，预热 0.5 秒、采样约 3 秒，保留原工作量及协议断言。以下是 Criterion 中心估计值。

| 丢包率 / 本项载荷 | 修改前 | 修改后 |
| --- | ---: | ---: |
| 0% / 4 MiB | 3.5311 ms | 2.3948 ms |
| 20% / 12 KiB | 17.858 µs | 14.117 µs |
| 40% / 2 KiB | 11.111 µs | 9.7551 µs |
| 60% / 160 B | 2.4498 µs | 2.0498 µs |
| 80% / 32 B | 179.19 µs | 165.66 µs |

命令为 `cargo bench --locked --bench kcp-transfer -- --save-baseline before-std-api` 与
`cargo bench --locked --bench kcp-transfer -- --baseline before-std-api`。
五项本轮均未见回退；不同丢包率采用不同载荷，不能横向比较吞吐，不能将单轮本机内存链路
观测推导为公网吞吐或跨平台性能承诺。显式克隆 `Vec<u8>` 会深拷贝；借用输入转为独立载荷、
多段组装和报文编码仍需复制。

未验证 Windows 或公网互通；高层仍依赖 Tokio 运行时，依赖兼容性只对实际受测版本作结论。

署名：GPT-6-Astra-Ultra
