# v0.7.0

高层消息 API 新增有期限的协议确认等待和显式 KCP 参数配置，应用可在库管理的 UDP 连接上控制发送收尾、MTU、窗口与重传策略。

## 主要变化

- 客户端新增 `drain(timeout).await`，监听器新增 `drain_to(peer, timeout).await`：等待此前成功入队的数据获得 KCP 累计确认，并输出当前待发确认和窗口通告。总期限包含请求排队；取消或超时后可继续收发或再次等待。
- 新增 `connect_with_kcp_config(addr, stream_config, kcp_config)` 和 `bind_with_kcp_config(...)`，保留旧入口的 fast 默认值及 `StreamConfig` 字段；支持消息模式的 MTU、窗口、normal/fast 与自定义重传配置。
- 高层构造在创建连接前拒绝非法 MTU、零窗口和字节流模式，发送按实际 MSS 校验 255 分片上限，避免准入与后台配置不一致。
- 文件传输示例双方在退出前显式确认，移除固定等待；接收端同步文件内容、重命名后发送业务回执；结束确认失败时区分文件保存状态与结束确认状态。
- 客户端不再将系统拒绝的 UDP 输出报告为成功；监听器等待实际 socket 输出完成后才推进确认状态。

## 升级注意

- Cargo 依赖更新为 `kcp-ovo = "0.7.0"`，Rust 最低版本仍为 1.99.1。
- 新增 `KcpError::TimedOut` 和 `KcpErrorKind::TimedOut`；下游穷尽匹配需要增加对应分支。
- close/Drop 保持立即终止，发送成功仍只表示进入本地队列。协议确认不证明对端业务处理或落盘；超时不证明数据未到达，不能据此盲目重发非幂等操作。
- 最后一份确认丢失且对端退出时，另一端仍可能超时；会话替换不会完成旧会话的等待，不提供握手、自动重连或新的投递保证。
- 高层只支持消息模式，MTU 为 `29..=65507` 字节并包含 28 字节 KCP 头；UDP 上限不保证路径可用。两端不协商参数，单条消息分片数仍须符合对端接收窗口，更大数据应由应用分块。
- 既有线格式、默认参数和 feature 保持不变，没有新增依赖。

使用方法见 [API 指南](https://github.com/cherish-ltt/kcp-ovo/blob/v0.7.0/docs/api-guide.md)，验证范围与工具限制见 [v0.7.0 验证记录](https://github.com/cherish-ltt/kcp-ovo/blob/v0.7.0/docs/stream-v0.7.0.md)。

关联 Issue：[#41](https://github.com/cherish-ltt/kcp-ovo/issues/41)、[#40](https://github.com/cherish-ltt/kcp-ovo/issues/40)

署名：GPT-6-Astra-Ultra
