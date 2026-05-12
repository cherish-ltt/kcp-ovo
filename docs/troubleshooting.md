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
use kcp_ovo::KcpStream;

match KcpStream::connect("127.0.0.1:8888") {
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
use kcp_ovo::{KcpStream, StreamConfig};
use std::time::Duration;

let config = StreamConfig {
    connect_timeout: Duration::from_secs(10),  // 增加超时时间
    ..Default::default()
};

match KcpStream::connect_with_config("127.0.0.1:8888", config) {
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
    fn keep_alive(&mut self) -> KcpResult<()> {
        let now = Instant::now();
        if now.duration_since(self.last_update) >= self.update_interval {
            self.kcp.update(get_current_ms());
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
match kcp.recv(&mut buffer) {
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
use kcp_ovo::Kcp;

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
            match kcp.recv(&mut buffer) {
                Ok(n) => {
                    println!("接收到: {}", String::from_utf8_lossy(&buffer[..n]));
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
kcp.update(current_time);
```

3. 检查队列状态:
```rust
println!("RCV queue: {}", kcp.nrcv_que);
println!("RCV buf: {}", kcp.nrcv_buf);
```

### 问题2: 数据发送不出去

**症状**:
```rust
kcp.send(data)?;
// 但对端收不到
```

**可能原因**:
1. 未设置output回调
2. 未调用flush()
3. socket未正确配置

**诊断**:

```rust
use kcp_ovo::Kcp;

let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;

// 设置output回调
let sent_count = std::sync::Arc::new(std::sync::Mutex::new(0));
let sent_clone = sent_count.clone();

kcp.set_output(move |data, _kcp| {
    println!("发送 {} 字节", data.len());
    *sent_clone.lock().unwrap() += 1;
    socket.send(data)?;
    Ok(data.len())
});

// 发送数据
kcp.send(b"Hello")?;

// 检查队列
println!("SND queue: {}", kcp.nsnd_que);
println!("SND buf: {}", kcp.nsnd_buf);
println!("实际发送: {}", *sent_count.lock().unwrap());

// 必须调用flush()！
kcp.flush();
```

**解决方案**:

1. 必须设置output回调:
```rust
kcp.set_output(|data, _| {
    socket.send(data)?;
    Ok(data.len())
});
```

2. 发送后必须flush():
```rust
kcp.send(data)?;
kcp.flush();  // 重要！
```

3. 检查socket是否连接:
```rust
println!("Socket connected: {}", socket.peer_addr().is_ok());
```

### 问题3: 数据乱序

**症状**:
接收到的数据顺序与发送不一致

**原因**:
这是KCP的正常行为，KCP会处理乱序并重组

**验证**:

```rust
// 发送
kcp.send(b"Msg1")?;
kcp.send(b"Msg2")?;
kcp.send(b"Msg3")?;
kcp.flush();

// 接收
let mut msg1 = [0u8; 4];
let mut msg2 = [0u8; 4];
let mut msg3 = [0u8; 4];

// 注意：可能需要多次recv才能收到完整消息
kcp.recv(&mut msg1)?;  // "Msg1"
kcp.recv(&mut msg2)?;  // "Msg2"
kcp.recv(&mut msg3)?;  // "Msg3"
```

**如果需要消息边界**:

使用流式模式或添加消息长度头:

```rust
fn send_message(kcp: &mut Kcp, msg: &[u8]) -> KcpResult<()> {
    // 发送长度前缀
    let len = msg.len() as u32;
    kcp.send(&len.to_be_bytes())?;

    // 发送消息体
    kcp.send(msg)?;
    kcp.flush()
}

fn recv_message(kcp: &mut Kcp) -> KcpResult<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    kcp.recv(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;

    let mut msg = vec![0u8; len];
    kcp.recv(&mut msg)?;
    Ok(msg)
}
```

## 性能问题

### 问题1: 高延迟

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
    kcp.update(get_current_ms());
}

let elapsed = start.elapsed();
println!("1000次update耗时: {:?}", elapsed);
```

**解决方案**:

```rust
// 降低更新频率
kcp.interval = 100;  // 从10ms改为100ms

// 或者使用Stream API的自动管理
use kcp_ovo::{KcpStream, StreamConfig};
use std::time::Duration;

let config = StreamConfig {
    update_interval: Duration::from_millis(50),
    auto_update: true,  // 自动管理
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
kcp-ovo = { version = "0.1", features = ["stream"] }
```

或使用默认配置:
```toml
[dependencies]
kcp-ovo = "0.1"  # stream已默认启用
```

### 问题2: 链接错误

**错误**:
```
error: linking with `cc` failed
```

**原因**: mimalloc链接问题

**解决方案**:

```toml
[dependencies]
mimalloc = { version = "0.1", default-features = false }
```

或禁用mimalloc:
```rust
// 注释掉 lib.rs 中的全局分配器
// #[global_allocator]
// static GLOBAL: MiMalloc = MiMalloc;
```

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
let n = kcp.recv(&mut buffer).unwrap();

// 正确做法
match kcp.recv(&mut buffer) {
    Ok(n) => println!("收到 {} 字节", n),
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

// KCP不是线程安全的！
let kcp = Arc::new(Mutex::new(Kcp::new(conv, config)?));

// 正确的使用方式
{
    let mut kcp = kcp.lock().unwrap();
    kcp.send(data)?;
    kcp.flush();
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
cargo flamegraph --example stream-api

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
use kcp_ovo::{Kcp, KcpConfig};

fn main() -> kcp_ovo::KcpResult<()> {
    let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;

    // 设置输出
    kcp.set_output(|data, _| Ok(data.len()))?;

    // 问题代码
    kcp.send(b"test")?;
    kcp.recv(&mut [0u8; 1024])?;

    Ok(())
}
```

## 调试checklist

- [ ] 检查conv ID是否匹配
- [ ] 确认设置了output回调
- [ ] 验证调用了flush()
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
