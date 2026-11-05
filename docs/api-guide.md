# KCP-OVO API 指南

## 模块与类型

- `kcp_ovo::stream::{KcpStream, KcpListener, StreamConfig}`：默认 `stream` feature 提供的异步消息 API。
- `kcp_ovo::core::kcp::Kcp`：底层协议控制块。
- `kcp_ovo::{KcpConfig, KcpError, KcpResult}`：协议配置和错误类型。

Stream API 不实现 `std::io::Read` / `Write`，监听器不提供 `accept` 或 `try_accept`。完整可运行的双进程示例见 [README](../README.md#-快速开始)。

## 客户端与监听器

`KcpStream::connect(addr).await` 或 `connect_with_config(addr, config).await` 创建客户端。`send(&[u8]).await` 入队消息，`recv().await` 返回一条消息；`try_send` / `try_recv` 不等待。`remote_addr` 返回所选地址，`is_connected` 表示本地任务仍在运行，并不证明远端可达。主机名使用解析出的第一个地址，不自动尝试后续地址。

`KcpListener::bind(addr).await` 或 `bind_with_config(addr, config).await` 创建监听器；`local_addr` 返回实际绑定地址。`recv().await` 返回 `(Vec<u8>, SocketAddr)`，使用 `send_to(&data, addr).await` 回复已有会话。两者的 `close` 会停止后台任务并丢弃待处理数据，不等待发送确认。

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
