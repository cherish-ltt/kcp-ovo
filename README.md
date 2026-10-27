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
- ✅ **标准trait** — 实现Read/Write trait
- ✅ **开箱即用** — 几行代码即可使用

### 底层API
- ✅ **完全控制** — 精确控制KCP行为
- ✅ **零开销** — 无额外抽象层
- ✅ **易于集成** — 适合集成到现有事件循环

## 🚀 快速开始

### 安装

在`Cargo.toml`中添加：

```toml
[dependencies]
kcp-ovo = "0.3"
```

### Stream API（推荐）

#### Echo服务器

```rust
use kcp_ovo::KcpListener;
use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let mut listener = KcpListener::bind("0.0.0.0:8888").await?;
    println!("服务器启动在 0.0.0.0:8888");

    Ok(())
}
```

#### Echo客户端

```rust
use kcp_ovo::KcpStream;
use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let mut stream = KcpStream::connect("127.0.0.1:8888").await?;
    stream.send(b"Hello, KCP!").await?;

    let bytes = stream.recv().await?;
    println!("收到: {}", String::from_utf8_lossy(&bytes));
    Ok(())
}
```

### 底层API

```rust
use kcp_ovo::{Kcp, KcpConfig};

fn main() -> kcp_ovo::KcpResult<()> {
    // 创建KCP实例
    let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;

    // 设置输出回调
    kcp.set_output(|data| {
        async move {
            // 通过UDP socket发送
            udp_socket.send_to(data, &remote_addr)?;
            Ok(data.len())
        }
    });

    // 发送数据
    kcp.send(b"Hello, KCP!")?;
    kcp.flush();

    // 接收UDP数据并输入到KCP
    let (len, _) = udp_socket.recv_from(&mut buf)?;
    kcp.input(&buf[..len])?;

    // 更新KCP状态
    kcp.update(current_timestamp_ms);

    // 接收数据
    let bytes = kcp.recv()?;
    println!("收到: {}", String::from_utf8_lossy(&bytes));

    Ok(())
}
```

### 流模式使用

```rust
use kcp_ovo::{Kcp, KcpConfig};

// 启用流模式（一行代码）
let config = KcpConfig::stream_mode();

// 或通过 struct 字面量
let config = KcpConfig {
    stream_mode: true,
    ..KcpConfig::fast_mode()
};

let mut kcp = Kcp::new(0x11223344, config)?;

// 流模式下可发送任意大小的数据（不再受255分片限制）
let large_data = vec![0u8; 1_000_000]; // 1MB
kcp.send(&large_data)?;
kcp.flush();

// 接收端一次性读回全部字节，无消息边界
let bytes = kcp.recv()?;
assert_eq!(bytes.len(), 1_000_000);
```

> **注意**：流模式与非流模式**不能互连互通**，两端的 `stream_mode` 必须一致。

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
> 实测 512 段已接近本机吞吐饱和点。窗口按段计，单段载荷约 `mtu - 24` 字节。

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
| `stream-easy-api` | Echo服务器/客户端 | `cargo run --example stream-easy-api` |
| `low-level-api` | 底层API演示 | `cargo run --example low-level-api` |
| `file-transfer` | 文件传输示例 | `cargo run --example file-transfer -- [send\|recv] <file>` |

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
| **优点** | 简单易用，几行代码即可使用；自动管理KCP状态更新；实现标准IO trait；自动处理socket读写 |

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
kcp-ovo = "0.3"

# 仅使用底层API
kcp-ovo = { version = "0.3", default-features = false }

# 显式启用Stream API
kcp-ovo = { version = "0.3", features = ["stream"] }
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

## 🔄 开发路线图

### 已完成 ✅

- [x] Phase 1: 项目初始化和基础结构
- [x] Phase 2: 核心KCP协议实现
- [x] Phase 3: 测试基础设施
  - [x] 单元测试（50个测试）
  - [x] 测试依赖配置
- [x] Phase 4: Stream API实现
  - [x] KcpStream客户端封装
  - [x] KcpListener服务端封装
- [x] Phase 5: 文档和示例
  - [x] 入门教程
  - [x] API指南
  - [x] 性能优化指南
  - [x] 故障排查指南
  - [x] 3个示例程序
- [x] **Phase 6: 流模式（Stream Mode）**
  - [x] KcpConfig 新增 `stream_mode` 字段
  - [x] KcpConfig::stream_mode() 便捷构造方法
  - [x] send() 流模式分支（所有分片 frg=0，无 255 上限）
  - [x] recv() 流模式分支（一次性读取所有 segment 拼接）
  - [x] peeksize() 流模式分支（返回所有 segment 总和）
  - [x] 流模式测试覆盖
- [x] **Phase 7: 协议正确性修复（2026-05-22）**
  - [x] BUG-1: cwnd 初始化 0→IKCP_THRESH_INIT
  - [x] BUG-2: parse_ack RTT 计算 `>=`→`==`
  - [x] BUG-3: snd_una 从未更新（已修复）
  - [x] BUG-4: ACK 被 need_wask else 分支阻塞
  - [x] BUG-5: 超时+快速重传双重发送（加 else）
  - [x] BUG-6: rmt_wnd 初始值错误
  - [x] BUG-7: parse_fastack 调用顺序错误
  - [x] BUG-8: parse_ack O(n) 弹出-重新压入（改用 retain）
  - [x] BUG-9: 流模式 peeksize 容量不足
  - [x] BUG-10: 魔数 66601 替换
  - [x] BUG-11: 窗口通告用可用窗口代替总窗口
  - [x] 11 个 Bug 回归测试（50 个测试全部通过）

### 计划中 📋

- [ ] Phase 8: 集成测试和边界测试
- [ ] Phase 9: 性能基准测试
- [ ] 更多……

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
