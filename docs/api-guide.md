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

## StreamConfig

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `update_interval` | 10 ms | 后台更新间隔，必须非零 |
| `connect_timeout` | 5 s | 客户端输出操作的超时 |
| `max_sessions` | 1024 | 监听器会话上限，必须非零 |
| `session_idle_timeout` | 300 s | 会话无有效入站报文时的回收期限，必须非零 |

使用结构体字面量时可用 `..Default::default()` 填充其余字段。空闲回收可能丢弃尚未交付的数据，客户端需要重新连接。

## 底层 Kcp

`Kcp::new(conv, config)` 创建控制块，`set_output` 注册异步发包回调；`send` 入队，`input` 输入收到的 UDP 报文，`update(current).await` 推进协议，`check(current)` 返回下次调度时间，`peeksize` / `recv` 读取数据，`waitsnd` 查看待发送段数。双方必须约定兼容的会话和协议配置。完整签名以 `cargo doc --open` 和源码为准，本地示例见 [low-level-api.rs](../examples/low-level-api.rs)。

`KcpConfig::default()` 为消息模式；`fast_mode()`、`fast_mode_with_wnd()` 和 `stream_mode()` 是预设。底层流模式不保留消息边界，通信两端必须使用相同模式；高层 `KcpStream` 使用消息模式，不能通过 `StreamConfig` 切换到底层流模式。
