# v0.9.0 高层 API 验证记录

本次完成 [Issue #48](https://github.com/cherish-ltt/kcp-ovo/issues/48)、
[#49](https://github.com/cherish-ltt/kcp-ovo/issues/49)、
[#50](https://github.com/cherish-ltt/kcp-ovo/issues/50)。原始基线为 v0.8.0 的
`7d5e2cd81c7340ea3c13d169873d218f08f955cb`；验证日期为 2026-11-06，
本地环境为 macOS 27.2、aarch64-apple-darwin、Rust 1.99.1。

三项功能分别经 [PR #51](https://github.com/cherish-ltt/kcp-ovo/pull/51)、
[#52](https://github.com/cherish-ltt/kcp-ovo/pull/52)、
[#53](https://github.com/cherish-ltt/kcp-ovo/pull/53) Squash 合并，功能整合提交为
`24ac9573c9269b146efb3eb49658b194bdc96ecc`。每条 PR 均通过 Linux Rust CI 的格式、
Clippy、编译、构建、全量测试、无默认 feature 和独立下游检查。

## 可观察差异

| 同一操作 | v0.8.0 实际结果 | v0.9.0 实际结果 |
| --- | --- | --- |
| 高层查询 RTT、窗口及待确认段数 | 验收调用编译失败，缺少诊断方法（E0599） | 返回带真实采样时间的完整快照；未知值、有效零值与过期快照分别验证，锁竞争立即报错 |
| 原对象接收时由独立任务发送并等待确认 | 验收调用编译失败，缺少 `sender()`（E0599） | 客户端和监听器发送句柄可移入独立任务，按实际入队顺序收发，owner 关闭使遗留句柄失效 |
| 会话已满时按业务回收其中一个 | 验收调用编译失败，缺少 `close_session`（E0599） | 停止旧来源重发后主动回收立即释放名额，新来源准入；监听地址及其他会话保持可用 |
| 捕获旧实例的操作在同地址重建后执行 | 无单会话主动回收入口 | 旧关闭、发送和确认等待不作用于新实例；旧积压消息与排队输出失效，新消息精确交付 |

这些是新增接口与边界验证，不构成吞吐改善或生产容量收益的证明。底层线格式、默认参数、
原有高层调用方式和立即 close/Drop 保持兼容；仍需遵守 [v0.8.0 迁移](api-guide.md#v080-迁移)。

## 验证范围

- 三项功能整合后，`cargo test --workspace --all-features` 为 **405 passed、0 failed、6 ignored**，
  包含 401 项单元/集成测试和 4 项文档测试；ignored 为 5 项既有文档示例和 1 项手动采样成本测试。
- 全目标 check、Clippy（`-D warnings`）、workspace build、fmt 和严格 rustdoc 检查通过。
- `cargo test --locked --no-default-features --test api_contract` 的 32 项通过，保留显式 System
  全局分配器及标准库公开类型兼容性。
- 独立 `tests/downstream/core/` 与 `tests/downstream/stream/` 各 2 项通过，两个项目的 fmt/Clippy
  也通过；stream 使用 Tokio 1.54.1 / bytes 1.12.1，验证实际 UDP 收发、诊断、发送句柄及回收 future。
- 高层五套定向黑盒共 64 项通过；新增覆盖保留字节完整、消息边界、窗口、固定种子网络损伤、
  黑洞及恢复。发送句柄与单会话关闭均覆盖 20 客户端和超过 1 MiB 数据，既有协议仿真继续覆盖
  恶意输入、序号回绕和非对称配置。
- 关闭边界同时验证待入队操作失败、已提交成功不追溯改变、取消与 owner Drop 唤醒；真实 UDP
  调度用例验证旧实例排队输出被丢弃、同地址新实例输出成功。1000 次创建/回收验证旧交付标记释放。
- 临时恢复阻塞式生命周期检查时，诊断锁竞争回归实际失败（退出码 101）；恢复非阻塞检查后通过。
  新接口的基线验收均实际报告 E0599，未把尚不存在的功能当作已有缺陷。
- 最终整合的 `prek run --all-files` 全部适用钩子通过，包含密钥扫描、ast-grep 与 Rust
  fmt/check/Clippy/全目标测试；图片、JSON 检查因无匹配文件跳过。工具报告旧 Node 缓存不匹配，
  最终 ast-grep 已实际运行并通过。开发期间 ast-grep 曾拒绝测试中的 `let-else panic`；
  测试改为返回 `Result` 并保留失败分支和所有断言后通过，未压制规则或放宽验收。

## 发布准备

版本元数据和根目录、两个独立下游锁文件中的本库版本一致为 0.9.0，其他依赖未变。
版本更新后重新执行全量测试仍为 405 passed、0 failed、6 ignored；无默认 feature 32 项、
两个独立下游各 2 项及各自 fmt/Clippy、严格 rustdoc 与完整 prek 均通过。版本分支未修改
产品或测试源码，覆盖率采用功能整合版本的实际报告，未重复测量。
`cargo publish --dry-run --allow-dirty --registry crates-io` 已完成打包和编译验证；本机镜像配置
要求显式指定 crates.io。Cargo 提示锁文件中的既有间接依赖 `chacha20 0.10.0` 已被撤回，
本次未更新该依赖；预演成功不代表已上传。正式发布沿用 `release.yml` 的标签验证、GitHub
Release、crates.io OIDC 可信发布顺序。

## 覆盖率

`cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 80` 返回 0，
总行覆盖率 **95.00%**（2979 行、149 行未覆盖），区域覆盖率 95.21%，函数覆盖率 92.97%。
报告伴随 `211 functions have mismatched data` 警告；未隐藏警告、过滤生产文件或降低 80% 门槛。
这不是无警告的覆盖率证明，本轮未重复对 LLVM 映射逐符号诊断。

## 性能与适用范围

诊断功能提交时显式运行 release 模式手动测试，1,000,000 次循环测得约 **33 ns/采样、9 ns/查询**。
这是本机无竞争缓存操作观测，不是端到端延迟，也不承诺多线程竞争或其他平台的成本。
本次未重新开展核心 Criterion 性能对比，不将 v0.8.0 的历史性能数字作为本版本结果。

诊断只返回最近完整快照，不保证新鲜或远端在线；底层段数不包含高层通道积压。单会话回收是本地
立即终止，可能丢弃未完成数据；不能撤回已进入内核或网络的报文，也不阻止来源重新建立会话。
发送成功不代表送达，KCP 确认成功不代表对端应用已处理或持久化。

`sender-handles` 已实际运行完成双端并发收发；`session-close` 只完成编译和对应 API 场景验证，
未启动常驻示例服务。Windows、公网、生产负载及其他依赖版本未验证。

署名：GPT-6-Astra-Ultra
