# KCP-OVO 故障排查指南

本指南帮助您诊断和解决使用kcp-ovo时遇到的问题。(注意维护时间，可能为旧版本)

## 目录

- [连接问题](#连接问题)
- [数据传输问题](#数据传输问题)
- [性能问题](#性能问题)
- [编译问题](#编译问题)
- [运行时错误](#运行时错误)
- [调试技巧](#调试技巧)

## 连接问题

### 问题1: 无法连接到服务器

**症状**:
```
Error: IoError: Connection refused
```

**可能原因**:
1. 服务器未启动
2. 端口被占用
3. 防火墙阻止
4. 网络不可达

**诊断步骤**:

```rust
use kcp_ovo::stream::KcpStream;

match KcpStream::connect("127.0.0.1:8888").await {
    Ok(_) => println!("连接成功"),
    Err(e) => {
        eprintln!("连接失败: {}", e);
        eprintln!("请检查:");
        eprintln!("1. 服务器是否运行?");
        eprintln!("2. 端口8888是否开放?");
        eprintln!("3. 防火墙设置?");
    }
}
```

**解决方案**:

1. 检查服务器是否运行:
```bash
netstat -an | grep 8888  # Linux/Mac
netstat -an | findstr 8888  # Windows
```

2. 测试网络连通性:
```bash
ping <server_ip>
telnet <server_ip> 8888
```

3. 检查防火墙:
```bash
# Linux
sudo iptables -L -n

# Mac
sudo pfctl -s rules

# Windows
netsh advfirewall show allprofiles
```

### 问题2: 连接超时

**症状**:
```
Error: IoError: Connection timed out
```

**可能原因**:
1. 网络延迟过高
2. 丢包严重
3. conv ID不匹配
4. 未正确调用update()

**诊断**:

```rust
use kcp_ovo::stream::{KcpStream, StreamConfig};
use std::time::Duration;

let config = StreamConfig {
    connect_timeout: Duration::from_secs(10),  // UDP 输出操作的超时
    ..Default::default()
};

match KcpStream::connect_with_config("127.0.0.1:8888", config).await {
    Ok(stream) => println!("连接成功"),
    Err(e) => {
        eprintln!("连接超时: {}", e);
        eprintln!("建议:");
        eprintln!("1. 检查网络延迟: ping <server_ip>");
        eprintln!("2. 检查丢包率");
        eprintln!("3. 验证conv ID是否匹配");
    }
}
```

**解决方案**:

1. 检查网络质量:
```bash
ping -c 100 <server_ip> | grep "packet loss"
```

2. 验证conv ID:
```rust
println!("Client conv: 0x{:08X}", client_conv);
println!("Server conv: 0x{:08X}", server_conv);
// 两者必须一致！
```

3. 启用日志:
```rust
kcp.set_log(|msg, _| {
    println!("[KCP] {}", msg);
});
```

### 问题3: 频繁断线

**症状**:
连接建立后很快断开

**可能原因**:
1. 未定期调用update()
2. 超时时间过短
3. 网络不稳定

**解决方案**:

```rust
use std::time::{Duration, Instant};

struct KcpConnection {
    kcp: Kcp,
    last_update: Instant,
    update_interval: Duration,
}

impl KcpConnection {
    async fn keep_alive(&mut self) -> KcpResult<()> {
        let now = Instant::now();
        if now.duration_since(self.last_update) >= self.update_interval {
            self.kcp.update(get_current_ms()).await?;
            self.last_update = now;
        }
        Ok(())
    }
}
```

## 数据传输问题

### 问题1: 无法接收数据

**症状**:
```rust
match kcp.recv() {
    Err(KcpError::QueueEmpty) => {
        println!("队列为空");
    }
}
```

**可能原因**:
1. 未调用input()
2. conv ID不匹配
3. 数据包格式错误

**诊断**:

```rust
use kcp_ovo::core::kcp::Kcp;

let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;

// 模拟接收UDP数据
loop {
    let mut udp_buffer = [0u8; 65536];
    match socket.recv_from(&mut udp_buffer) {
        Ok((len, src)) => {
            println!("收到 {} 字节来自 {}", len, src);

            // 检查数据是否正确输入
            match kcp.input(&udp_buffer[..len]) {
                Ok(_) => println!("输入成功"),
                Err(e) => eprintln!("输入失败: {}", e),
            }

            // 尝试接收
            match kcp.recv() {
                Ok(data) => {
                    println!("接收到: {}", String::from_utf8_lossy(&data));
                    break;
                }
                Err(KcpError::QueueEmpty) => {
                    println!("队列为空，继续等待...");
                }
                Err(e) => {
                    eprintln!("接收错误: {}", e);
                    break;
                }
            }
        }
        Err(e) => {
            eprintln!("UDP接收错误: {}", e);
            break;
        }
    }
}
```

**解决方案**:

1. 确保调用input():
```rust
// 每次从socket收到数据后
kcp.input(&udp_packet)?;
```

2. 确保定期调用update():
```rust
kcp.update(current_time).await?;
```

3. 检查队列状态:
```rust
println!("RCV queue: {}", kcp.nrcv_que);
println!("RCV buf: {}", kcp.nrcv_buf);
```

### 问题2: 数据发送不出去

`send(&[u8])` 只把数据加入发送队列。底层 API 需要设置输出回调，并持续调用
`update(current).await` 驱动输出、确认和重传，同时将收到的 UDP 报文交给 `input`。
`flush` 是私有实现，调用方不应直接使用。

输出回调接收拥有所有权的 `Vec<u8>`，返回异步发送结果。例如，`socket` 是已连接的
`Arc<tokio::net::UdpSocket>` 时：

```rust
kcp.set_output(move |data: Vec<u8>| {
    let socket = socket.clone();
    async move { socket.send(&data).await.map_err(Into::into) }
});
kcp.send(b"Hello")?;
kcp.update(current_time).await?;
```

回调可以直接把 `data` 移入异步任务，无需转换成 `Bytes` 或先复制一份。发送失败必须返回错误，
不能仅打印日志并返回成功。通过 `waitsnd()` 观察待确认段数；高层 API 使用
`drain(timeout).await` 或 `drain_to(peer, timeout).await` 等待协议确认。

### 问题3: 消息顺序或边界与预期不符

底层消息模式按发送顺序交付完整消息，每次 `recv()` 返回一个 `Vec<u8>`；网络报文乱序
不会使应用消息乱序。接收不完整消息时会返回 `IncompleteData`，不能将其当作完整消息处理。

```rust
kcp.send(b"Msg1")?;
kcp.send(b"Msg2")?;
// 持续驱动两端 update/input 后，在接收端逐条读取：
let msg1: Vec<u8> = receiver.recv()?;
let msg2: Vec<u8> = receiver.recv()?;
assert_eq!(msg1, b"Msg1");
assert_eq!(msg2, b"Msg2");
```

`KcpConfig::stream_mode()` 不保留消息边界，一次 `recv()` 会拼接当前所有可读段。
需要消息边界时使用消息模式，或在应用协议中维护持久接收缓冲区并解析长度帧；
不能假定一次流式接收恰好返回一个长度头或一条业务消息。高层 `KcpStream` 使用消息模式。

## 性能问题

### 问题1: 高延迟

#### 高层协议诊断

使用自动驱动接口时，无须自行持有底层 `Kcp`：

```rust
fn diagnose(stream: &kcp_ovo::stream::KcpStream) -> kcp_ovo::KcpResult<()> {
    let snapshot = stream.diagnostics()?;
    println!("采样年龄: {} ms", snapshot.sampled_at.elapsed().as_millis());
    println!("平滑 RTT: {:?} ms", snapshot.smoothed_rtt_ms);
    println!("重传超时估计: {} ms", snapshot.retransmission_timeout_ms);
    println!("远端窗口: {:?} 段", snapshot.remote_window_segments);
    println!("拥塞窗口: {} 段", snapshot.congestion_window_segments);
    println!("KCP 待发/在途: {}/{} 段", snapshot.queued_send_segments, snapshot.in_flight_segments);
    Ok(())
}
```

监听器使用 `listener.diagnostics_for(peer_addr)` 查询该地址当前实例。
查询只复制最近的一次完整采样，不读取消息、不输出报文，也不等待 `drain`。
关闭、任务退出、会话不存在或缓存竞争时返回错误；调用方可在自己的采样周期重试。
后台处理每轮事件后尝试刷新缓存，但 UDP 输出挂起时缓存年龄会持续增加，
因此排障必须先看 `sampled_at`，不能把旧状态视为即时状态。

`smoothed_rtt_ms = None` 表示从未取得有效 RTT 样本，重传段不参加 RTT 采样；
有效的零毫秒样本为 `Some(0)`。`remote_window_segments = None` 表示尚未收到对端报文，
`Some(0)` 才表示对端通告零窗口。RTO 是本地估计；窗口和队列单位都是段，
两种发送段数之和等于底层 `waitsnd()`，不包含尚在高层发送通道中的数据。
禁用拥塞控制时拥塞窗口不限制发送。这些状态不能单独证明对端在线、应用已读或故障根因。

**症状**:
延迟 > 200ms

**诊断**:

```rust
println!("=== KCP状态 ===");
println!("RTT: {} ms", kcp.rx_srtt);
println!("RTO: {} ms", kcp.rx_rto);
println!("SND queue: {}", kcp.waitsnd());
println!("Interval: {} ms", kcp.interval);
println!("Nodelay: {}", kcp.nodelay);
```

**解决方案**:

```rust
// 切换到低延迟配置
let config = kcp_ovo::KcpConfig::fast_mode();
let mut kcp = Kcp::new(conv, config)?;
```

### 问题2: 低吞吐量

**症状**:
吞吐量 < 1Mbps

**诊断**:

```rust
println!("=== 窗口状态 ===");
println!("CWND: {}", kcp.cwnd);
println!("RMT window: {}", kcp.rmt_wnd);
println!("RCV window: {}", kcp.rcv_wnd);
println!("有效窗口: {}", kcp.cwnd.min(kcp.rmt_wnd));
```

**解决方案**:

```rust
// 增大窗口
kcp.rcv_wnd = 512;

// 禁用拥塞控制（内网）
kcp.nocwnd = true;

// 增大MTU
let config = kcp_ovo::KcpConfig {
    mtu: 9000,
    ..Default::default()
};
```

### 问题3: 高CPU使用

**症状**:
CPU使用率 > 50%

**诊断**:

```rust
use std::time::Instant;

let start = Instant::now();

// 执行1000次update
for _ in 0..1000 {
    kcp.update(get_current_ms()).await?;
}

let elapsed = start.elapsed();
println!("1000次update耗时: {:?}", elapsed);
```

**解决方案**:

```rust
// 降低更新频率
kcp.interval = 100;  // 从10ms改为100ms

// 或者使用Stream API的自动管理
use kcp_ovo::stream::{KcpStream, StreamConfig};
use std::time::Duration;

let config = StreamConfig {
    update_interval: Duration::from_millis(50),
    ..Default::default()
};
```

## 编译问题

### 问题1: Stream API不存在

**错误**:
```
error[E0433]: failed to resolve: use of undeclared crate or module `stream`
```

**原因**: stream feature未启用

**解决方案**:

```toml
[dependencies]
kcp-ovo = { version = "0.9.0", features = ["stream"] }
```

或使用默认配置:
```toml
[dependencies]
kcp-ovo = "0.9.0"  # stream已默认启用
```

### 问题2: 全局分配器冲突

**错误**:
```
error: the `#[global_allocator]` in this crate conflicts with global allocator in: kcp_ovo
```

**原因**: v0.5.0 在库中声明全局 mimalloc，与应用自身的全局分配器冲突；
关闭默认 feature 不能消除该声明。

**解决方案**: 升级到 v0.5.1 或更新版本，并更新依赖锁文件：

```toml
[dependencies]
kcp-ovo = "0.9.0"
```

```bash
cargo update -p kcp-ovo
```

v0.5.1 不再依赖或声明 mimalloc，无需修改库源码。默认配置与仅底层 API 配置均尊重
应用的分配器选择；应用没有声明全局分配器时使用 Rust 默认分配器。
若应用需要 mimalloc，应自行添加依赖并只在最终程序中声明全局分配器。

### 问题3: Tokio/bytes 导入、类型或版本冲突

v0.8.0 移除了 `kcp_ovo::tokio` 和 `kcp_ovo::bytes`。应用仍需 Tokio 时，直接在自己的
`Cargo.toml` 中声明并使用 `tokio`；高层 API 仍需 Tokio 运行时。底层 `recv`、输出回调、
包数据和编码结果改为 `Vec<u8>`，不再要求下游导入 bytes 类型。

如果出现 `failed to select a version`，检查 `cargo tree` 和双方依赖约束。内部 Tokio/bytes
分别使用兼容范围 `1.52.3` / `1.11.1`，可以与满足该范围的较新版本统一解析；
更旧的精确版本或其他不相交约束仍可能冲突。仅移除重导出无法消除精确版本约束造成的冲突。

地址参数现在满足标准库 `ToSocketAddrs + Send + 'static`。遇到局部字符串“不满足 `'static`”时，
改为移动 `String`（需要保留原值时显式克隆）、传入 `(String, u16)` 或预解析的 `SocketAddr`：

```rust
let address = format!("{host}:{port}");
let stream = kcp_ovo::stream::KcpStream::connect(address).await?;
```

包解析请改用 `KcpPacket::from_bytes(&mut encoded.as_slice())`；`Segment::get_data()` 返回
借用切片，需在段释放后保留数据时使用 `.to_vec()`。完整语义见 [v0.8.0 迁移](api-guide.md#v080-迁移)。

## 运行时错误

### 问题1: panic in recv

**错误**:
```rust
thread 'main' panicked at 'called `Result::unwrap()` on an `Err` value'
```

**原因**: 未正确处理错误

**解决方案**:

```rust
// 错误做法
let data = kcp.recv().unwrap();

// 正确做法
match kcp.recv() {
    Ok(data) => println!("收到 {} 字节", data.len()),
    Err(KcpError::QueueEmpty) => {
        println!("队列为空，正常情况");
    }
    Err(e) => {
        eprintln!("错误: {}", e);
    }
}
```

### 问题2: 死锁

**症状**:
程序卡住不动

**原因**: 在多线程环境中错误使用KCP

**解决方案**:

```rust
use std::sync::{Arc, Mutex};

// 同一控制块的可变操作需要串行执行。
let kcp = Arc::new(Mutex::new(Kcp::new(conv, config)?));

// 正确的使用方式
{
    let mut kcp = kcp.lock().unwrap();
    kcp.send(data)?;
}  // 锁在此释放

// 不要嵌套锁
{
    let mut kcp = kcp.lock().unwrap();
    // 不要在这里再次获取锁
}
```

### 问题3: 内存泄漏

**症状**:
内存使用持续增长

**诊断**:

```bash
# Linux/Mac
valgrind --leak-check=full --show-leak-kinds=all cargo run

# 或使用heap profiler
cargo install heaptrack
heaptrack cargo run
```

**解决方案**:

```rust
// 确保及时清理
struct Connection {
    kcp: Option<Kcp>,
}

impl Connection {
    fn close(&mut self) {
        // 显式清理
        self.kcp = None;
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.kcp = None;
    }
}
```

## 调试技巧

### 1. 启用日志

```rust
kcp.set_log(|msg, kcp| {
    println!("[KCP] {} | snd_q={} rtt={} rto={}",
        msg,
        kcp.waitsnd(),
        kcp.rx_srtt,
        kcp.rx_rto
);
});

// 设置日志掩码
kcp.logmask = 0xFFFFFFFF;  // 所有日志
```

### 2. 统计信息

```rust
struct KcpStats {
    packets_sent: u64,
    packets_recv: u64,
    bytes_sent: u64,
    bytes_recv: u64,
    retransmissions: u64,
}

impl KcpStats {
    fn print(&self) {
        println!("=== KCP统计 ===");
        println!("发送包数: {}", self.packets_sent);
        println!("接收包数: {}", self.packets_recv);
        println!("发送字节: {}", self.bytes_sent);
        println!("接收字节: {}", self.bytes_recv);
        println!("重传次数: {}", self.retransmissions);

        let total = self.packets_sent + self.packets_recv;
        if total > 0 {
            let loss_rate = (self.retransmissions as f64) / (total as f64);
            println!("丢包率: {:.2}%", loss_rate * 100.0);
        }
    }
}
```

### 3. 抓包分析

```bash
# 使用tcpdump抓包
sudo tcpdump -i any -w kcp.pcap port 8888

# 使用Wireshark分析
wireshark kcp.pcap

# 过滤KCP包
# udp.port == 8888
```

### 4. 性能分析

```bash
# CPU性能分析
cargo install flamegraph
cargo flamegraph --example stream-easy-api

# 生成火焰图
# 打开 flamegraph.svg
```

## 常见错误代码

| 错误 | 原因 | 解决方案 |
|------|------|---------|
| `QueueEmpty` | 接收队列为空 | 等待更多数据或检查input() |
| `BufferTooSmall` | 缓冲区太小 | 增大缓冲区或调用peeksize() |
| `IncompleteData` | 数据不完整 | 等待完整数据包 |
| `InvalidSequence` | 序列号错误 | 检查conv ID是否匹配 |
| `OutputNotSet` | 未设置输出 | 调用set_output() |
| `IoError` | IO错误 | 检查socket状态 |

## 获取帮助

### 1. 查看文档

```bash
# 查看API文档
cargo doc --open

# 查看示例
cargo run --example low-level-api
```

### 2. 启用调试日志

```rust
// 设置环境变量
std::env::set_var("RUST_LOG", "debug");
env_logger::init();
```

### 3. 社区支持

- GitHub Issues: [kcp-ovo/issues](https://github.com/cherish-ltt/kcp-ovo/issues)
- 查看已有Issue
- 提交新的Bug报告

### 4. 最小复现示例

提交Bug时，请提供最小复现示例：

```rust
use kcp_ovo::{core::kcp::Kcp, KcpConfig};

fn main() -> kcp_ovo::KcpResult<()> {
    let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;

    // 设置输出
    kcp.set_output(|data: Vec<u8>| async move { Ok(data.len()) });

    // 问题代码
    kcp.send(b"test")?;
    let received: Vec<u8> = kcp.recv()?;
    println!("收到 {} 字节", received.len());

    Ok(())
}
```

## 调试checklist

- [ ] 检查conv ID是否匹配
- [ ] 确认设置了output回调
- [ ] 确认定期调用update()
- [ ] 检查网络连通性
- [ ] 验证防火墙设置
- [ ] 启用日志输出
- [ ] 查看队列状态
- [ ] 检查错误返回值
- [ ] 抓包分析

## 参考资源

- [KCP协议Wiki](https://github.com/skywind3000/kcp/wiki)
- [Rust错误处理](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [网络调试工具](https://www.wireshark.org/docs/)
