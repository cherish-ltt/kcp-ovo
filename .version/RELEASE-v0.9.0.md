# v0.9.0

高层消息 API 新增协议诊断、独立发送句柄和单会话主动回收，方便应用组织并发收发、排查等待原因及释放会话资源。

## 主要变化

- `KcpStream::diagnostics()` 与 `KcpListener::diagnostics_for(peer)` 返回最近完整的 `KcpDiagnostics`，包含 RTT、RTO、远端窗口、拥塞窗口和底层发送段数。查询不等待锁；用 `sampled_at` 判断快照年龄，未知 RTT/远端窗口与有效零值明确区分。
- `sender()` 返回可克隆的 `KcpSender` / `KcpListenerSender`，可移入独立任务，与接收 owner 并发工作。发送和 `drain` / `drain_to` 复用原有有界 FIFO；监听器句柄只向已有会话发送。
- `KcpListener::close_session(peer)` 仅回收调用时选中的本地会话，释放容量并终止该实例未完成的发送和确认等待；旧接收积压与排队输出失效，其他会话继续运行。
- 新增双端并发发送、单会话回收示例，以及诊断、并发、背压、关闭竞争和同地址重建的黑盒与独立下游验证。

## 升级注意

- Cargo 依赖更新为 `kcp-ovo = "0.9.0"`；Rust 最低版本仍为 1.99.1。既有高层 API、消息边界、默认参数及协议线格式保持兼容。
- 高层接口仍需要 Tokio 运行时，公开类型继续使用标准库和本库类型。由 v0.7.x 或更早版本升级时，仍需完成 [v0.8.0 API 迁移](https://github.com/cherish-ltt/kcp-ovo/blob/v0.9.0/docs/api-guide.md#v080-迁移)。
- 诊断是缓存快照；段数不包含高层发送通道，RTO 不等于实测 RTT，成功查询不证明远端在线。
- 原对象保留唯一接收和生命周期所有权。丢弃发送句柄不关闭连接；owner close/Drop 仍立即终止，即使遗留句柄仍然存在。多生产者按实际入队顺序排列，发送成功不保证数据送达，确认等待成功不代表应用处理或持久化。
- 单会话回收不通知或封禁对端，后续合法报文可立即重建会话；已经交给内核或网络的报文不能撤回，已提交的成功结果保持原样。需要收尾时先有界等待 `drain_to`，再由应用决定回收。

用法见 [API 指南](https://github.com/cherish-ltt/kcp-ovo/blob/v0.9.0/docs/api-guide.md)，验证结果与限制见 [v0.9.0 验证记录](https://github.com/cherish-ltt/kcp-ovo/blob/v0.9.0/docs/high-level-v0.9.0.md)。

关联 Issue：[#48](https://github.com/cherish-ltt/kcp-ovo/issues/48)、[#49](https://github.com/cherish-ltt/kcp-ovo/issues/49)、[#50](https://github.com/cherish-ltt/kcp-ovo/issues/50)

署名：GPT-6-Astra-Ultra
