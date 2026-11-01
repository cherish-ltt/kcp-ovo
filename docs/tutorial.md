# KCP-OVO 入门教程

## 安装与 Echo

按 [README 快速开始](../README.md#-快速开始) 添加 `kcp-ovo` 和 Tokio 依赖，复制其中的服务端和客户端，或在仓库中运行：

```bash
# 终端 1
cargo run --example echo-server
# 终端 2
cargo run --example echo-client
```

客户端应输出 `收到: Hello, KCP!`。服务端用 `recv().await` 取得消息与来源地址，用 `send_to().await` 回发；客户端用 `send().await` 和 `recv().await`。这些调用返回 `KcpResult`。服务端持续运行，可通过 Ctrl-C 结束。

`connect` 创建并连接 UDP socket，不等待服务器握手。发送成功只表示数据进入本地队列，应用需要自己的回复、超时和重试策略来确认业务完成。示例是演示代码，没有认证或加密。

## 配置与生命周期

`KcpStream::connect_with_config` 和 `KcpListener::bind_with_config` 接受 `StreamConfig`；`update_interval` 必须非零。监听器默认最多保留 1024 个会话，300 秒未收到有效报文后回收；可用 `max_sessions` 和 `session_idle_timeout` 修改，均须非零。会话回收可能丢弃尚未交付的数据，客户端需要重新连接。

`close()` 或丢弃对象会停止后台任务，丢弃待发送和未读取的数据，不等待对端确认。等待应用层回复后再关闭。`try_send` 和 `try_recv` 是客户端的非阻塞接口，队列满或无数据时返回错误。

## 底层接口

使用 `kcp_ovo::core::kcp::Kcp` 和 `KcpConfig` 可以自行管理 UDP：配置输出回调、将收到的数据交给 `input`，按时间调用异步 `update`，再通过 `recv` 取出消息。运行 `cargo run --example low-level-api` 查看 API；该示例仅演示本地队列，未建立网络通信。

底层 `KcpConfig::stream_mode()` 使用无消息边界的字节流，两端必须一致；高层 `KcpStream` 使用消息模式，名称相似但不是同一设置。消息模式最多 255 个分片；接收窗口不足以容纳完整消息时可能无法交付。

更多方法和配置见 [API 指南](api-guide.md)。
