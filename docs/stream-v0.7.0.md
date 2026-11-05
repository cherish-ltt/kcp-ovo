# v0.7.0 高层消息接口验证记录

本记录覆盖 [#41](https://github.com/cherish-ltt/kcp-ovo/issues/41) 的有界确认等待与 [#40](https://github.com/cherish-ltt/kcp-ovo/issues/40) 的协议配置入口。验证日期为 2026-11-05，基线为 v0.6.0（`a528dee`），最终整合受测提交为 `d2029270dfe43c8958a13648e51a353a2d15fe6a`。

基线上调用新增确认等待或配置构造入口均编译报 E0599；整合后通过下述验收。原有 `send` 仅保证本地入队、close / Drop 立即终止和默认 fast 配置保持原契约。

## 验收矩阵

[#41 黑盒测试](../tests/stream_drain.rs) 14 项通过；序号回绕、后台退出和输出错误另由库单测验证。

| 验收点 | 实测结果 |
| --- | --- |
| 空闲、FIFO 与确认边界 | 空队列可完成；立即 send 后 drain 不提前成功；丢 ACK 或仅部分确认时继续等待，之后发送的数据不延长此前屏障。 |
| 期限、取消与恢复 | 控制请求排队计入总期限；取消或超时保留已入队数据。永久黑洞和零窗口返回 `TimedOut`，恢复确认或窗口后可再次 drain。 |
| 关闭与会话身份 | close / Drop、后台退出唤醒等待并返回 `IoError`；同地址新会话不能完成旧 future，调用时不存在的会话不能被后来创建的会话补上。 |
| 背压与隔离 | 一个监听会话停滞并有接收积压时，另外 20 个客户端仍完成双向收发；客户端积压的 65 条消息按序读出，协议确认继续推进。 |
| 当前确认信息输出 | 在回环测试中，drain 后立即 close，raw peer 收到最后 ACK；读完背压积压后，须先输出携带最新累计确认的 WINS。此测试证明本地输出边界，不保证真实网络中最后确认必达。 |
| 序号回绕与错误传播 | `u32::MAX` 边界的发送数据和待发确认完成后才成功；真实 UDP send 错误返回 `IoError`。 |
| 规模与损伤网络 | 固定 seed 下传输 1024 条、每条 1024 字节，共 1 MiB；注入丢包、延迟、抖动、乱序、重复和起始黑洞，逐条逐字节校验，并断言损伤实际发生。 |
| 文件收尾 | [文件示例](../examples/file-transfer.rs) 覆盖空文件、多块文件、非法输入保护、最终 BYE/ACK 丢失、提前退出和临时文件清理，保存内容逐字节一致。 |

[#40 配置及组合测试](../tests/stream_config.rs) 7 项通过。

| 验收点 | 实测结果 |
| --- | --- |
| 构造期校验 | 非法 MTU、零窗口或字节流模式在地址解析前返回 `InvalidConfig`；MTU 29 和 65507 的合法构造通过。 |
| 配置矩阵和默认行为 | 11 组消息模式配置完成双向回传，含 normal/fast、重传、更新间隔、非对称 MTU/窗口/拥塞配置；抓包验证实际 MTU、在途窗口和接收窗口通告。原有构造保持 fast 默认。 |
| 消息准入 | MTU 29、128、1400、1500 下，`255 * MSS` 字节正常传输，多 1 字节在 send / try_send / send_to 入队前拒绝；拒绝前后合法消息仍按序、按原边界交付。 |
| 损伤、规模与并发 | normal/fast 在固定 seed 的丢包、抖动、乱序、重复和黑洞中恢复；20 客户端使用非对称 MTU，共发送 1.25 MiB 请求数据并逐块回显。 |
| 配置与 drain 组合 | `asymmetric_mtu_drains_both_directions_after_blackhole_timeout` 使用 normal↔fast、MTU 128/576 和不同窗口；双向实际黑洞均先返回 `TimedOut`，解除后原 2301/5001 字节消息及后续消息完整交付，双方确认等待完成后关闭。 |

## 负向对照

下列用例实际恢复旧逻辑并失败，恢复修正后通过，证明断言能够识别问题。

| 用例 | 恢复旧逻辑后的表现 | 修正后 |
| --- | --- | --- |
| `udp_output_preserves_socket_error` | 真实 UDP 发送 65508 字节，系统错误被包装成 `Ok(65508)`，断言失败。 | 返回 `IoError`。 |
| `barrier_waits_for_cumulative_confirmation_after_receive_backpressure` 及对应高层回归 | 仅检查发送队列和 ACK 队列时，最新累计确认尚未输出却返回 `Ok(())`，单元和高层回归均失败。 | 将待发 WINS 纳入完成条件；193 条消息读完、drain 并立即 close 后，raw peer 收到 `una=193`。 |
| `maximum_message_and_rejected_send_preserve_order` | 恢复固定 MSS=1372，MTU 29 下错误接受超限消息，用例退出码 101。 | 使用实际 MSS 后同一用例退出码 0，255 分片边界及消息顺序成立。 |

## 整合验证结果

下列结果均对应最终整合受测提交，命令退出码均为 0。

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check`；全目标、全 feature Clippy（`-D warnings`）；workspace check / build | 通过。 |
| `prek run --all-files` | 适用 hooks 全部通过，包含全目标测试；无适用图片或 JSON 文件的检查显示 `Skipped`。 |
| `cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 80` | 24 套测试、368 项单元及集成测试通过，0 failed、0 ignored。行覆盖率 **95.05%**（2221 行，110 行未覆盖），区域覆盖率 95.59%，函数覆盖率 93.36%；有 **148 条函数数据不匹配警告**。 |
| 配置、drain 和示例定向测试 | 7 + 14 + 3 项通过；前两组已计入上述 368 项。 |
| `cargo test --doc --all-features` | 4 项通过，5 项已有文档示例标记 ignored。 |

去重后共 **375 项通过**，另有 **5 项文档示例未执行**。本地整合未重复运行无默认 feature 检查，整合后的 Linux CI 已执行并通过。stable rustfmt 对已有 nightly 专用配置输出提示；hooks 有旧 Node 缓存版本提示，但实际 ast-grep 检查通过。

[#41 PR #43](https://github.com/cherish-ltt/kcp-ovo/pull/43) 已合并为 `d69bde2`；[Linux CI](https://github.com/cherish-ltt/kcp-ovo/actions/runs/41992054427) 在该 PR head `4d73433` 上通过。

[#40 PR #44](https://github.com/cherish-ltt/kcp-ovo/pull/44) 已合并为 `1d983eb`；[整合 Linux CI](https://github.com/cherish-ltt/kcp-ovo/actions/runs/41994143249) 在 head `d202927` 上通过，包含格式、Clippy、check、build、全量测试和 `cargo test --locked --no-default-features --test api_contract`。版本标签发布还须通过仓库 Release 工作流的独立验证；本记录不代替发布状态。

## 发布准备

版本号、安装示例和发布说明更新至 0.7.0；不改运行时代码。严格文档构建 `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` 通过；过程中修正了 `recv` 注释中被识别成 HTML 的 `Vec<u8>` 和已不存在的缓冲区参数说明。

`cargo publish --dry-run --allow-dirty --registry crates-io` 已完成打包及编译验证；77 个包文件包含新增实现、注册的黑盒测试、公共 UDP 代理和发布文档。锁文件中的间接依赖 `chacha20 0.10.0` 产生已撤回版本提示，打包验证成功；本次保留锁定依赖，不将 dry-run 视为真实发布。

## 覆盖率警告的解释与限制

最终报告超过 80% 门槛，但 148 条警告尚未逐符号核对，不能称为无警告或已证明完全不影响计数。

整合前的一轮报告曾确认：重复警告来自 hash 为 0 的未使用函数映射，对应真实非零 hash 和执行计数仍存在，改变测试目标读取顺序后该轮报告不变。独立最小项目也复现了同类警告和少计现象，因此保留报告值与限制，不将早期诊断直接套用于最终 148 条警告。

官方机制参考：Rust [`covfun.rs:65`](https://github.com/rust-lang/rust/blob/main/compiler/rustc_codegen_llvm/src/coverageinfo/mapgen/covfun.rs#L65) 将未使用映射的 hash 设为 0，[`unused.rs:13–20`](https://github.com/rust-lang/rust/blob/main/compiler/rustc_codegen_llvm/src/coverageinfo/mapgen/unused.rs#L13-L20) 说明为何保留此类映射；LLVM [`CoverageMapping.cpp:884–890`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/ProfileData/Coverage/CoverageMapping.cpp#L884-L890) 在 hash 不匹配时记录并跳过该映射。这些链接是官方 `main`，用于说明机制，**不是本机工具链的精确版本源码**。

## 适用范围

本地环境为 macOS 27.2、Apple Silicon（`aarch64-apple-darwin`）、Rust 1.99.1、LLVM 23.1.1、cargo-llvm-cov 0.9.1。协议核心使用虚拟时钟，高层使用真实回环 UDP、固定 seed 损伤代理和有界等待；固定 seed 不保证操作系统调度完全一致。

未验证 Windows、公网/NAT、路径 MTU 探测、跨机器互通、独立 CLI 进程的文件收尾或生产性能。较大合法 MTU 的构造通过，不代表实际路径可承载相同大小的 UDP 数据报；本记录不承诺吞吐收益。

`drain` 成功表示此前成功入队的数据已获得 KCP 确认，当前待发确认信息已完成本地 socket 输出；不代表对端业务已读取、落盘，也不保证最后一份 ACK 在网络中送达。即使双方都调用 drain，最后确认丢失且一端退出时，另一端仍可能超时；超时不能证明对端未收到。close / Drop 仍立即终止。

署名：GPT-6-Astra-Ultra
