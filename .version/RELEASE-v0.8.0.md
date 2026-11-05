# v0.8.0

公开数据接口改用 Rust 标准库类型，移除 Tokio/bytes 重导出，并解除这两个内部依赖的精确版本约束，方便下游自行选择兼容版本。

## 主要变化

- `Kcp::recv`、输出回调、`KcpPacket::data` 与包编码结果使用 `Vec<u8>`；`Segment::get_data()` 返回 `&[u8]`，Header/Packet 解析接受 `&mut &[u8]`。
- 不再提供 `kcp_ovo::tokio` / `kcp_ovo::bytes`；六个高层 connect/bind 入口采用 `std::net::ToSocketAddrs + Send + 'static`，在后台阻塞线程解析地址。
- 内部 Tokio/bytes 依赖分别采用兼容范围 `1.52.3` / `1.11.1`，允许下游统一到满足范围的较新版本；独立下游项目使用 Tokio 1.54.1 / bytes 1.12.1 完成真实 UDP 回环收发。
- 载荷直接由 `Vec<u8>` 持有，单段接收、输出回调及内部通道转交时移动已有分配；编码借用队列载荷，避免克隆整个数据段。
- 增加独立下游编译、收发、标准地址解析与所有权回归验证，并纳入 CI 和发布前校验。

## 升级注意

- 这是破坏性 API 更新；依赖更新为 `kcp-ovo = "0.8.0"`，Rust 最低版本仍为 1.99.1。
- 使用高层接口的应用仍需自行声明 Tokio，并在 Tokio 运行时内调用；本次不提供运行时无关的高层接口，也不保证任意旧版或其他主版本依赖可用。
- 局部字符串借用改为移动 `String`、`(String, u16)` 或传入 `SocketAddr`；字符串字面量仍可直接传入。客户端仍仅使用首个解析地址，监听器按顺序尝试绑定。
- Header 解析返回借用的剩余切片；Packet 复制声明长度的载荷取得独立所有权。两者均只推进头部，不消费载荷；短头不推进，完整头但短载荷的 Packet 错误会保留头部推进。
- `freeze()` 仍返回 `Bytes`，不能直接作为 `Vec<u8>` 返回。借用输入取得独立所有权、多段组装及线格式编码保留必要复制，不承诺端到端零复制。显式克隆 Packet/Segment/队列现在会复制载荷。
- 线格式、默认协议参数、确认等待和关闭语义保持不变。

完整迁移表见 [API 指南](https://github.com/cherish-ltt/kcp-ovo/blob/v0.8.0/docs/api-guide.md#v080-迁移)，实测结果与工具限制见 [v0.8.0 验证记录](https://github.com/cherish-ltt/kcp-ovo/blob/v0.8.0/docs/public-api-v0.8.0.md)。

关联 Issue：[#46](https://github.com/cherish-ltt/kcp-ovo/issues/46)

署名：GPT-6-Astra-Ultra
