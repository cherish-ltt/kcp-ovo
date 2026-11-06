# KCP-OVO API 指南

## 模块与类型

- `kcp_ovo::stream::{KcpStream, KcpListener, StreamConfig}`：默认 `stream` feature 提供的异步消息 API。
- `kcp_ovo::core::kcp::Kcp`：底层协议控制块。
- `kcp_ovo::{KcpConfig, KcpError, KcpResult}`：协议配置和错误类型。

Stream API 不实现 `std::io::Read` / `Write`，监听器不提供 `accept` 或 `try_accept`。完整可运行的双进程示例见 [README](../README.md#-快速开始)。

## 客户端与监听器

`KcpStream::connect(addr).await` 或 `connect_with_config(addr, config).await` 创建客户端。`send(&[u8]).await` 入队消息，`recv().await` 返回一条 `Vec<u8>` 消息；`try_send` / `try_recv` 不等待。`remote_addr` 返回所选地址，`is_connected` 表示本地任务仍在运行，并不证明远端可达。主机名使用解析出的第一个地址，不自动尝试后续地址。

`KcpListener::bind(addr).await` 或 `bind_with_config(addr, config).await` 创建监听器；`local_addr` 返回实际绑定地址。`recv().await` 返回 `(Vec<u8>, SocketAddr)`，使用 `send_to(&data, addr).await` 回复已有会话。两者的 `close` 会停止后台任务并丢弃待处理数据，不等待发送确认。

所有 connect/bind 构造函数的地址参数满足 `std::net::ToSocketAddrs + Send + 'static`，
支持标准库地址类型及自定义实现。地址解析在后台执行，不阻塞异步执行线程；
监听器按解析顺序尝试绑定，客户端仍只使用第一个地址。字符串字面量可以直接传入，
非静态借用应改为拥有所有权的 `String`、`(String, u16)` 或预解析的 `SocketAddr`。
高层操作需要 Tokio 运行时，但公开签名不包含 Tokio 类型或 trait。

发送成功表示消息进入本地队列，而不是对端收到。消息模式单次发送最多 255 个分片，接收窗口必须足以容纳完整消息。UDP 和 KCP 不提供应用层身份认证、加密或业务交付确认。

## 有期限的协议确认等待

客户端使用 `stream.drain(Duration::from_secs(5)).await?`，监听器使用 `listener.drain_to(peer, Duration::from_secs(5)).await?`。等待请求与发送数据共用有界 FIFO，因此包含此前 `send` / `try_send` / `send_to` 成功入队、尚未交给底层 KCP 的数据。等待期间后台持续收包、确认和重传，后续发送暂存在有界队列中，不延长已经开始的等待；监听器的其他会话独立运行。

总期限从 future 首次轮询开始，包含请求入队及确认等待。成功表示此前数据获得 KCP 累计确认，且当前已经处理的待发 ACK 与窗口通告已成功输出到本地 UDP socket；不保证 ACK 在网络中送达，也不代表远端应用读取、处理或持久化了数据。仍未处理的 UDP 输入不包含在成功判定中。

- `KcpError::TimedOut` / `KcpErrorKind::TimedOut` 表示总期限耗尽；数据可能已到达，确认包可能丢失。
- close、Drop、后台任务退出或监听器回收会话时，未完成等待返回 `IoError`。空闲客户端无需远端在线即可完成等待，因此成功不是连通性检测。
- 取消 future 或超时只停止等待，不撤回已经接受的数据。控制请求被后台清理，后续收发与再次等待仍可继续。
- future 在调用时捕获连接或会话，可单独等待并继续通过原对象 `recv`、`send` 或 `close`。同地址后来创建的新会话不会完成旧等待；不提供会话协商或自动重连。
- `close` 和 Drop 保持立即终止。需要业务确认时先完成应用协议，再显式等待协议确认；双端收尾见 [文件示例](../examples/file-transfer.rs)。最后一个 ACK 丢失且对端退出时，另一端仍可能超时。

新增错误枚举成员后，下游穷尽匹配需要补充 `TimedOut` 分支。

## 主动回收单个会话

`listener.close_session(peer).await?` 返回 `true` 时，调用创建时选中的会话已移除并释放
`max_sessions` 名额；不存在或已经移除返回 `false`。future 可移入独立任务，创建后延迟
执行仍只关闭原实例，不会关闭同地址的新会话。监听器关闭或任务停止返回错误。

操作与监听器实际 UDP 输出串行，完成后旧实例排队输出失效；已经交给内核或网络的包无法
撤回。尚未成功入队的发送和未完成的确认等待以错误结束，已经提交的成功结果保持原样。
该实例尚未交付给 `recv` 的消息（含共享接收队列积压）被丢弃，其他实例和同地址新实例
不受影响；已经完成的接收结果即使应用稍后处理也不撤回。空闲回收的既有消息交付行为不变。

这只终止本地会话，不通知或封禁对端；后续合法数据包可以立即重新占用名额。
取消尚未入队的关闭 future 没有副作用，取消已入队请求不会撤回关闭。确认此前数据时先
有界调用 `drain_to`；示例 [session-close.rs](../examples/session-close.rs) 在超时后明确
选择强制回收，应用也可以选择保留会话后重试。

## StreamConfig

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `update_interval` | 10 ms | 后台更新间隔，必须非零 |
| `connect_timeout` | 5 s | 客户端输出操作的超时 |
| `max_sessions` | 1024 | 监听器会话上限，必须非零 |
| `session_idle_timeout` | 300 s | 会话无有效入站报文时的回收期限，必须非零 |

使用结构体字面量时可用 `..Default::default()` 填充其余字段。空闲回收可能丢弃尚未交付的数据，客户端需要重新连接。

## 高层协议配置

`connect` / `connect_with_config` 和 `bind` / `bind_with_config` 保持 `KcpConfig::fast_mode()`。
需要调整协议时，分别使用 `KcpStream::connect_with_kcp_config(addr, stream_config, kcp_config)`
和 `KcpListener::bind_with_kcp_config(addr, stream_config, kcp_config)`；监听器为每个会话使用相同配置。
`StreamConfig` 的字段保持不变，原有完整结构体字面量仍可编译。

```rust
use kcp_ovo::{KcpConfig, stream::{KcpListener, KcpStream, StreamConfig}};

#[tokio::main]
async fn main() -> kcp_ovo::KcpResult<()> {
    let protocol = KcpConfig { mtu: 1200, ..KcpConfig::fast_mode_with_wnd(64, 128) };
    let listener = KcpListener::bind_with_kcp_config(
        "127.0.0.1:8888", StreamConfig::default(), protocol.clone(),
    ).await?;
    let _stream = KcpStream::connect_with_kcp_config(
        listener.local_addr(), StreamConfig::default(), protocol,
    ).await?;
    Ok(())
}
```

构造函数在解析地址或绑定 socket 前验证：MTU 为 `29..=65507` 字节、收发窗口非零、
`stream_mode=false`，不满足时返回 `KcpError::InvalidConfig`。MTU 包含 28 字节 KCP 头，
不包含 UDP/IP 头；65507 是本接口为 IPv4/IPv6 采用的共同 UDP 载荷上限，并不保证路径或操作系统接受该大小。
`normal_mode()`、`fast_mode()` 的重传/拥塞策略均可使用，底层字节流模式只通过 `Kcp` 使用。

`StreamConfig.update_interval` 是后台调用 `update` 的周期，`KcpConfig.interval` 是协议刷新周期；
把协议周期调得比后台周期短不会增加后台唤醒次数。参数不会自动协商，两端 MTU 和窗口可以不同，
但单条消息的分片数须能放入对端接收窗口，否则消息可能无法完成组装。
`send`、`try_send`、`send_to` 都按本端实际 `MSS = mtu - 28` 校验，最多发送 `255 * MSS` 字节，
超出时在入队前返回 `InvalidConfig`，不影响此前及后续合法消息；更大的数据应由应用分块。

## 底层 Kcp

`Kcp::new(conv, config)` 创建控制块，`set_output` 注册异步发包回调；`send` 入队，`input` 输入收到的 UDP 报文，`update(current).await` 推进协议，`check(current)` 返回下次调度时间，`peeksize` / `recv` 读取数据，`waitsnd` 查看待发送段数。双方必须约定兼容的会话和协议配置。完整签名以 `cargo doc --open` 和源码为准，本地示例见 [low-level-api.rs](../examples/low-level-api.rs)。

`KcpConfig::default()` 为消息模式；`fast_mode()`、`fast_mode_with_wnd()` 和 `stream_mode()` 是预设。底层流模式不保留消息边界，通信两端必须使用相同模式；高层 `KcpStream` 使用消息模式，不能通过 `StreamConfig` 切换到底层流模式。

## v0.8.0 迁移

| v0.7.x 用法 | v0.8.0 用法 |
| --- | --- |
| `kcp_ovo::tokio` / `kcp_ovo::bytes` | 应用需要时直接声明并导入 `tokio` / `bytes`，本库不再重导出 |
| `Kcp::recv() -> KcpResult<Bytes>` | 返回 `KcpResult<Vec<u8>>`，可直接交给接收 `Vec<u8>` 的代码 |
| `set_output` 的 `Bytes` 参数 | 回调接收 `Vec<u8>`；可移动到异步任务，不必 `.to_vec()` |
| `KcpPacket::data` / `new_with_data` 的 `Bytes` | 使用拥有所有权的 `Vec<u8>` |
| Header/Packet 的 `to_bytes` 返回 `BytesMut` / `Bytes` | 均返回 `Vec<u8>` |
| Header/Packet 的 `from_bytes(&mut Bytes)` | 改为 `from_bytes(&mut &[u8])` |
| `Segment::get_data() -> Bytes` | 返回 `&[u8]`；独立保留载荷时使用 `.to_vec()` |
| 构造入口接受 Tokio 的 `ToSocketAddrs` | 改为标准库 `ToSocketAddrs + Send + 'static` |

Header 解析返回 `(KcpPacketHeader, &[u8])`，其中剩余切片借用原输入；Header 与 Packet
解析都只将输入推进 28 字节到载荷起点，不消费载荷。短头错误不推进输入；完整头但载荷不足时，
Packet 解析返回错误且输入已推进头部。Packet 仅复制头部声明长度的载荷，返回值独立于原输入存活。

```rust
use kcp_ovo::{KcpCmd, helper::{KcpPacket, KcpPacketHeader}};

let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 0, 0, 0, 4);
let encoded: Vec<u8> = KcpPacket::new_with_data(header, b"data".to_vec()).to_bytes()?;
let mut input = encoded.as_slice();
let packet = KcpPacket::from_bytes(&mut input)?;
assert_eq!(input, b"data");
assert_eq!(packet.data, b"data");
```

载荷内部直接持有 `Vec<u8>`，单段 `recv` 转移已有分配，输出回调与高层接收也直接移动缓冲区。
`freeze()` 的结果仍是 `Bytes`，不适用于要求 `Vec<u8>` 的公开边界；借用输入取得独立所有权、
多段组装及头部与载荷编码仍保留必要复制，不承诺端到端零复制。
显式克隆 `KcpPacket`、`Segment` 或含有载荷的 `KcpDeque` 会复制 `Vec<u8>` 内容，
不再共享引用计数载荷；只读取数据时优先借用，转交所有权时优先移动。

内部依赖使用 `tokio = "1.52.3"`、`bytes = "1.11.1"` 的 Cargo 兼容范围，允许解析到
满足范围的较新版本；不要求下游精确使用库锁文件里的版本。范围不包含更旧版本或下一主版本，
高层 API 也没有变成与运行时无关的接口。仅使用底层 API 的应用无需直接依赖 Tokio 或 bytes。
