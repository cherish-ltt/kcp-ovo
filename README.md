<div align="center">
<h1>kcp-ovo</h1>
<p>
  <a href="https://github.com/cherish-ltt/kcp-ovo/actions/workflows/rust-ci.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/cherish-ltt/kcp-ovo/rust-ci.yml?branch=main" alt="Build Status"/>
  </a>
  <a href="https://crates.io/crates/kcp-ovo">
    <img src="https://img.shields.io/crates/v/kcp-ovo.svg" alt="crates.io version"/>
  </a>
  <a href="https://docs.rs/kcp-ovo">
    <img src="https://docs.rs/kcp-ovo/badge.svg" alt="documentation"/>
  </a>
  <a href="https://github.com/cherish-ltt/kcp-ovo/blob/main/LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="license"/>
  </a>
  <a href="https://www.rust-lang.org">
    <img src="https://img.shields.io/badge/rust-1.95.0+-orange.svg" alt="rustc"/>
  </a>
</p>
</div>


> 🚀 KCP协议的纯Rust实现 — 快速可靠的ARQ协议，比TCP降低30%-40%延迟

## 📖 简介

[kcp-ovo](https://github.com/cherish-ltt/kcp-ovo) 是一个快速可靠的ARQ（Automatic Repeat-reQuest）协议的纯Rust实现。KCP是一个低延迟、高可靠性的传输层协议，相比传统TCP可以降低**30%-40%的延迟**，最大RTT减少**三倍**。

本项目参考了[skywind3000/kcp](https://github.com/skywind3000/kcp)的原版C代码，使用纯Rust语言，充分利用Rust的类型安全和内存安全特性，同时提供类似TCP的Stream API简化使用。

## ✨ 特性

### 核心特性
- ✅ **纯Rust实现** — 无FFI依赖，完全使用Rust重写
- ✅ **内存优化** — 使用mimalloc全局分配器优化内存性能
- ✅ **类型安全** — 充分利用Rust类型系统，防止内存错误
- ✅ **零成本抽象** — 性能接近原版C实现
- ✅ **双API设计** — 提供底层API和Stream API（高级API）

### Stream Mode（流模式）
- ✅ **无消息边界** — 纯字节流语义，类似TCP，不再要求255分片大小限制
- ✅ **字节追加** — 接收端按序列号顺序连续拼接所有segment，一次性交付
- ✅ **大消息支持** — 任意大小数据均可发送，不受u8::MAX分片数约束
- ✅ **自动分片** — 超出MSS的数据自动切分，每个分片frg=0
- ✅ **便捷构造** — `KcpConfig::stream_mode()` 一行启用

### Stream API（推荐）
- ✅ **简单易用** — 类似TCP的使用体验
- ✅ **自动管理** — 自动处理update和socket读写
- ✅ **消息接口** — 使用异步 send/recv
- ✅ **开箱即用** — 几行代码即可使用

### 底层API
- ✅ **完全控制** — 精确控制KCP行为
- ✅ **零开销** — 无额外抽象层
- ✅ **易于集成** — 适合集成到现有事件循环

## 🚀 快速开始

`close()` 与释放 Stream/Listener 都会终止所属后台任务；关闭立即拒绝收发，丢弃待发送及未读取数据，不等待对端确认。需要可靠完成时，应先等待应用层完成回复。

Stream 的发送接口成功表示数据已进入本地队列，并非对端已收到。消息超过 255 个分片时会立即返回错误；接收端窗口小于消息分片数时，消息可能无法完成接收。

监听器默认最多保留 1024 个会话，并在 300 秒未收到有效报文后回收会话；可通过 `StreamConfig` 的 `max_sessions` 和 `session_idle_timeout` 调整，两者必须大于零，`update_interval` 也必须大于零。会话被回收后，空闲客户端需要重新连接。客户端按解析出的第一个地址选择 IPv4 或 IPv6，不会自动尝试后续地址。使用 `StreamConfig` 结构体字面量时，请通过 `..Default::default()` 填充未指定的字段。

### 安装

在`Cargo.toml`中添加：

```toml
[dependencies]
kcp-ovo = "0.5"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### Stream API（推荐）

#### Echo服务器

```rust
use kcp_ovo::stream::KcpListener;

#[tokio::main]
async fn main() -> kcp_ovo::KcpResult<()> {
    let mut listener = KcpListener::bind("127.0.0.1:8888").await?;
    loop {
        let (data, peer) = listener.recv().await?;
        listener.send_to(&data, peer).await?;
    }
}
```

#### Echo客户端

```rust
use kcp_ovo::stream::KcpStream;

#[tokio::main]
async fn main() -> kcp_ovo::KcpResult<()> {
    let mut stream = KcpStream::connect("127.0.0.1:8888").await?;
    stream.send(b"Hello, KCP!").await?;
    let reply = stream.recv().await?;
    println!("收到: {}", String::from_utf8_lossy(&reply));
    Ok(())
}
```

在两个终端分别运行 `cargo run --example echo-server` 和 `cargo run --example echo-client`，客户端应输出 `收到: Hello, KCP!`。完整源码在 [examples](examples/)；`send` 成功表示消息已进入本地队列，不表示对端已收到。

### 底层 API 与流模式

底层类型路径为 `kcp_ovo::core::kcp::Kcp`。请运行 `cargo run --example low-level-api` 查看配置、输出回调和队列用法；实际通信还需接收 UDP 数据并调用 `input`，定期异步调用 `update`。底层 `KcpConfig::stream_mode()` 提供无消息边界的字节流，两端必须采用相同模式；高层 `KcpStream` 使用消息模式，与这个配置不是同一设置。

## 📊 性能

### 性能对比

| 指标 | TCP | KCP (默认) | KCP (快速模式) | 提升 |
|------|-----|-----------|---------------|------|
| 平均RTT | 100ms | 60-70ms | 40-50ms | 30%-60% |
| 最大RTT | 300ms | 100ms | 90ms | **3倍** |
| 延迟波动 | 高 | 中 | 低 | 显著降低 |

### 适用场景

| 场景 | 推荐配置 | 预期延迟 |
|------|---------|---------|
| **游戏实时通信** | `fast_mode()` | 20-50ms |
| **视频直播** | `stream_mode()` | 100-200ms |
| **文件传输** | `stream_mode()` | 200-500ms |
| **IoT设备** | 自定义配置 | 可配置 |

## 🔧 配置选项

### 预定义配置

```rust
use kcp_ovo::KcpConfig;

// 默认配置（平衡模式）
let config = KcpConfig::default();

// 快速模式（最低延迟）
let config = KcpConfig::fast_mode();

// 流模式（无消息边界，无255分片限制，默认低延迟参数）
let config = KcpConfig::stream_mode();

// 快速模式 + 指定窗口：大流量场景下放大窗口以跑满带宽
// nocwnd=true 时拥塞窗口不参与调节，32 段的默认窗口就是吞吐硬上限
let config = KcpConfig::fast_mode_with_wnd(512, 512);

// 自定义配置
let config = KcpConfig {
    mtu: 1400,           // 最大传输单元
    interval: 20,        // 内部更新间隔(ms)
    nodelay: true,       // 无延迟模式
    fastresend: 2,       // 快速重传触发次数
    stream_mode: true,   // 流模式：所有分片 frg=0，无255分片上限
    nocwnd: true,        // 禁用拥塞控制
    rcv_wnd: 128,        // 接收窗口大小
    ..Default::default()
};
```

### 参数说明

| 参数 | 默认值 | 类型 | 说明 |
|------|--------|------|------|
| `mtu` | 1400 | u32 | 最大传输单元，影响单个包大小 |
| `interval` | 100 | u64 | 内部更新间隔(ms)，影响响应速度 |
| `nodelay` | false | bool | 是否启用无延迟模式 |
| `fastresend` | 0 | i32 | 快速重传触发次数 |
| `stream_mode` | false | bool | 流模式：所有分片 frg=0，无255分片上限 |
| `nocwnd` | false | bool | 是否禁用拥塞控制 |
| `rcv_wnd` | 128 | u16 | 接收窗口大小(segment数) |
| `snd_wnd` | 32 | u16 | 发送窗口大小(segment数) |

> `nocwnd=true`（`fast_mode()` 默认）时拥塞窗口被绕过，发送量只受
> `snd_wnd.min(rmt_wnd)` 限制，32 段即约 44KB 在途数据，会成为吞吐的硬上限。
> 需要跑满带宽请用 `KcpConfig::fast_mode_with_wnd(snd, rcv)` 显式放大，
> 实测 512 段已接近本机吞吐饱和点。窗口按段计，单段载荷约 `mtu - 28` 字节。

详细配置说明请参考：[性能优化指南](docs/performance.md)

## 📚 文档

- [入门教程](docs/tutorial.md) — 5分钟快速入门
- [API指南](docs/api-guide.md) — 完整API参考
- [性能优化](docs/performance.md) — 性能调优指南
- [故障排查](docs/troubleshooting.md) — 常见问题解决

生成API文档：

```bash
cargo doc --open
```

## 💡 示例程序

项目包含丰富的示例程序：

| 示例 | 说明 | 运行方式 |
|------|------|---------|
| `echo-server` / `echo-client` | 双进程 Echo | `cargo run --example echo-server` / `cargo run --example echo-client` |
| `stream-easy-api` | 同进程收发示例 | `cargo run --example stream-easy-api` |
| `low-level-api` | 底层API演示 | `cargo run --example low-level-api` |
| `file-transfer` | 文件传输示例 | `cargo run --example file-transfer -- [send\|recv] <file>` |

文件传输示例使用双方相同版本的应用协议，按块等待确认；接收端先写入目标目录的临时文件，完整接收后重命名到目标路径。目标已存在时的替换行为取决于操作系统；操作失败会返回错误。传输中断或确认超时会报错，接收端提交文件后的确认失败不代表文件未写入。

更多示例请查看 [examples/](examples/) 目录。

## 🏗️ 模块结构

```
src/
├── lib.rs          # 库入口，全局分配器
├── error.rs        # 错误类型定义
├── queue/          # 队列管理模块
│   ├── segment.rs  # 数据段结构
│   └── deque.rs    # 双向队列
├── helper/         # 编解码与常量模块
│   ├── mod.rs      # 模块入口
│   ├── packet.rs   # KcpPacket/KcpPacketHeader 编解码
│   ├── time.rs     # 时间戳工具
│   ├── kcp_cmd.rs  # KcpCmd 命令类型
│   └── kcp_const_config.rs  # KCP协议常量
├── config/         # 配置模块
│   └── params.rs   # KcpConfig 配置（含 stream_mode）
├── core/           # 核心协议模块
│   └── kcp.rs      # KCP控制块（主要实现，含流模式逻辑）
└── stream.rs       # Stream API（可选 feature）
```

## 🎯 API选择指南

### Stream API（推荐大多数应用）

| 维度 | 说明 |
|------|------|
| **何时使用** | 新项目开发、需要快速上手、类似TCP的使用体验 |
| **优点** | 简单易用，几行代码即可使用；自动管理KCP状态更新；异步发送和接收消息；自动处理socket读写 |

### 底层API（推荐高级用户）

| 维度 | 说明 |
|------|------|
| **何时使用** | 需要精细控制KCP行为、集成到现有事件循环、需要零开销抽象 |
| **优点** | 完全控制KCP行为（含 stream_mode）；精确管理update()时机；可以自定义输出逻辑；更灵活，无额外抽象层 |

### 流模式 vs 非流模式

| 维度 | 非流模式 (stream_mode=false) | 流模式 (stream_mode=true) |
|------|------------------------------|---------------------------|
| **消息边界** | 有，frg链组装完整消息 | **无**，纯字节流，类似TCP |
| **分片上限** | 255个分片（约350KB @1400MTU） | **无上限**，任意大小 |
| **frg字段** | 递减链（最后一个分片frg=0） | **所有分片 frg=0** |
| **peeksize** | 返回整个消息大小（等所有分片就绪） | **返回所有 segment 的字节总数** |
| **recv** | 读取到frg=0为止 | **读取所有可用segment** |
| **互连** | 可与非流模式对端通信 | **不能**与非流模式互连 |
| **适用场景** | 有明确消息边界的应用 | 大文件传输、视频流、字节流应用 |

## 📦 Feature Flags

| Feature | 默认启用 | 说明 |
|---------|---------|------|
| `stream` | ✅ 是 | Stream API高级封装，推荐使用 |

**使用方式**:

```toml
# 默认配置（包含Stream API）
kcp-ovo = "0.5"

# 仅使用底层API
kcp-ovo = { version = "0.5", default-features = false }

# 显式启用Stream API
kcp-ovo = { version = "0.5", features = ["stream"] }
```

## 🧪 测试

运行测试：

```bash
# 运行所有测试
cargo test

# 运行测试并显示输出
cargo test -- --nocapture

# 运行特定测试
cargo test test_stream_mode
```

测试覆盖：
- ✅ **核心单元测试**（Kcp创建、发送、接收、input、flush）
- ✅ **流模式测试**（分片全部为frg=0、无255上限、peeksize/recv语义、多次send内存连续交付）
- ✅ **Stream API测试**
- ✅ **协议正确性测试**（50 个测试用例全部通过，含 11 个 Bug 回归测试）
    - `test_cwnd_init_nonzero` — BUG-1：验证 cwnd 初始化为 `IKCP_THRESH_INIT`
    - `test_parse_ack_rtt_correct` — BUG-2：验证 RTT 使用正确段的 ts
    - `test_snd_una_updated` — BUG-3：验证 parse_una 更新 snd_una
    - `test_ack_sent_with_wask` — BUG-4：验证 Wask + ACK 独立发送
    - `test_no_dup_retransmit` — BUG-5：验证无重复重传
    - `test_rmt_wnd_zero_at_init` — BUG-6：验证 rmt_wnd 初始化为 0
    - `test_fastack_incremented` — BUG-7：验证快速重传正常生效
    - `test_parse_ack_nsnd_buf_integrity` — BUG-8：验证 retain 模式下 nsnd_buf 一致
    - `test_stream_peeksize_sum` — BUG-9：验证流模式 peeksize 返回所有 segment 总和
    - BUG-10：魔数已替换为有意义的错误消息（代码审查通过）
    - `test_no_deadlock_large_message` — BUG-11：验证 300KB 大文件传输无死锁
    - `test_default_mode_can_send` — BUG-1 补充：验证默认配置下数据可正常发送

### 黑盒测试

除单元测试外，`tests/` 下用虚拟链路跑真实 `Kcp` 实例做黑盒验证。
`tests/common/` 提供确定性仿真设施：`Rng`（固定种子）、`Net`（丢包/延迟/抖动/乱序/重复/突发丢包/延迟尖刺/路径 MTU）、
`Sim`（双端虚拟时钟）、`Sink`（单端抓包）。所有随机用例都由固定种子驱动，
失败时用同一 seed 即可复现；`tests/property.rs` 另有 proptest 在参数空间内自动搜索并收缩反例。
协议场景使用虚拟时间；Stream 场景另外使用本机 UDP 回环与有界超时。
2026-11-03 本机验证的 21 个集成测试套件运行时间合计约 4.1 秒，不含编译时间；
运行时间随机器与调度负载变化，覆盖和限制见 [黑盒验证记录](docs/blackbox-2026-11-03.md)。

黑盒测试是本项目的上线硬门槛，新增功能必须补齐对应场景才能合并，详见
[AGENTS.md](AGENTS.md) 第 10.1 节。

| 文件 | 关注点 |
| --- | --- |
| `transfer.rs` | 基本收发、模式差异、255 分片跨 MTU/丢包/黑洞交付、超限发送拒绝后恢复、peek 与不完整 recv 不消费消息 |
| `lossy.rs` | 丢包、乱序、重复下的可靠性 |
| `window.rs` | 收发窗口、零窗口、窗口通告、探测起步与退避上限、探测不挤占 ACK、长期零窗口后恢复 |
| `lifecycle.rs` | 创建、更新、关闭、死链 |
| `stream_api.rs` | `KcpStream` / `KcpListener` 接口、乱序交付、队列满后的非阻塞恢复、取消接收、跨会话背压隔离、满额活跃会话保留、外来 conv 隔离、地址族与关闭回收 |
| `api_contract.rs` | 公开 API 契约与错误类型 |
| `retransmit.rs` | 重传与死链判定 |
| `fuzz_transfer.rs` | 随机链路下 9 条协议不变式（字节精确、消息边界、窗口上限、断链恢复） |
| `malformed.rs` | 畸形包与恶意输入：任意字节、截断头、长度不符、未知命令、控制帧洪水、伪造 conv、畸形分片恢复；畸形 ACK 不丢待发消息，非法首包不绑定 conv |
| `matrix.rs` | 配置组合矩阵：MTU × 窗口 × interval × fastresend × nodelay × 流模式；nodelay 语义（最小 RTO、首传重传期限） |
| `timing.rs` | ACK 语义与时序：重复段补 ACK、取消安全、输出批次中途报错后数据与 ACK 重试、快速重传、探测退避、调度期限、确认边界与 RTT 方差收敛 |
| `congestion.rs` | 拥塞窗口：确认驱动增长、空闲与上界、慢启动、AIMD 收缩、`fastresend=0` 开关、三模式吞吐 |
| `soak.rs` | 规模与长跑：1MB~4MB 大流量、1000 轮长会话、SN 回绕（含双向跨回绕点、回绕叠加重传）、恶劣网络收敛、队列有界 |
| `outage.rs` | 断网与恢复：黑洞期补发、多次闪断、双向恢复、长断网不静默丢数据 |
| `multiclient.rs` | 多客户端并发：20~50 连接并发、部分断网隔离、混合丢包率、conv 不串台 |
| `udp_smoke.rs` | 真实 UDP 端到端冒烟：并发客户端、大载荷、异常关闭 |
| `adversarial.rs` | 敌意输入与账本边界：越过 `snd_nxt` 的伪造 `una`、在途段不得越过对端窗口、未知对端只放行一段探路、退化配置拒绝、头部极值扫描 |
| `property.rs` | 属性测试：任意字节/头部/配置不崩且账本自洽、随机链路（含小接收窗口）字节精确、双向同时传输 |
| `torture.rs` | 极端链路叠加：成簇丢包、延迟尖刺不引发重传风暴、路径 MTU 黑洞上报死链、双向同时断网、回绕叠加断网、100 连接、零窗口探测有界 |
| `udp_chaos.rs` | 真实 UDP 混沌：消息边界、垃圾报文、静默客户端隔离、40KB 消息；1/8/20 客户端持续精确回显与吞吐、RTT 分位数观测 |
| `smoke.rs` | 秒级冒烟：预设配置与矩阵互连、首包学习 conv 后完成请求/回复、空输入与退化配置、未知对端探路、控制帧往返、同刻重复 update 不重发 |

```bash
# 只跑黑盒测试
cargo test --test fuzz_transfer --test malformed --test matrix --test timing --test udp_smoke

# 新增的强化场景
cargo test --test adversarial --test property --test torture --test udp_chaos

# 秒级冒烟（改动后的第一道反馈）
cargo test --test smoke

# 本机回环负载观测，不代表公网性能或最大吞吐
cargo test --release --test udp_chaos sustained_loopback_load_is_byte_exact -- --exact --nocapture

# 覆盖率（需先安装 cargo-llvm-cov）
cargo llvm-cov --workspace --all-features --summary-only
```

## 📄 许可证

MIT License

本项目基于[skywind3000/kcp](https://github.com/skywind3000/kcp)（MIT License），使用相同的许可证。

详见 [LICENSE](LICENSE) 文件。

## 🙏 致谢

- [skywind3000/kcp](https://github.com/skywind3000/kcp) — 原版KCP协议实现
- [mimalloc-rust](https://github.com/purpleprotocol/mimalloc_rust) — Rust内存分配器
- [dashmap](https://github.com/xacrimon/dashmap) — 高效的并发 map
- [bytes](https://github.com/tokio-rs/bytes) — 字节处理工具库
- [tokio](https://github.com/tokio-rs/tokio) — 异步运行时
- 全部用到的 rust crates……

## 🤝 贡献

欢迎提交Issue和Pull Request！

### 贡献指南

1. Fork本仓库
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启Pull Request

### 开发规范

- 遵循 [AGENTS.md](AGENTS.md) 中的开发与黑盒测试要求
- 遵循Rust代码风格（`cargo fmt`）
- 通过Clippy检查（`cargo clippy`）
- 添加单元测试（`cargo test`）
- 更新相关文档

## 📮 联系方式

- GitHub: [cherish-ltt/kcp-ovo](https://github.com/cherish-ltt/kcp-ovo)
- Issues: [提交Issue](https://github.com/cherish-ltt/kcp-ovo/issues)

## 🌟 Star History

如果这个项目对你有帮助，请给一个Star⭐️

---

<div align="center">
  <sub>Built with ❤️ by the kcp-ovo team</sub>
</div>
