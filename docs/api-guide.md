# KCP-OVO API 指南

本指南详细介绍kcp-ovo的API使用方法。(注意维护时间，可能为旧版本)

## 目录

- [API概览](#api概览)
- [Stream API详解](#stream-api详解)
- [底层API详解](#底层api详解)
- [配置参数](#配置参数)
- [错误处理](#错误处理)
- [高级用法](#高级用法)

## API概览

kcp-ovo提供两个层次的API：

1. **Stream API** (高级API)
   - 简单易用
   - 自动管理
   - 类似TCP

2. **底层API**
   - 完全控制
   - 零开销
   - 灵活定制

### Stream API组件

```rust
// 类型
pub struct KcpStream;       // 客户端流
pub struct KcpListener;     // 服务端监听器
pub struct StreamConfig;    // 流配置

// trait实现
impl Read for KcpStream
impl Write for KcpStream
```

### 底层API组件

```rust
// 核心类型
pub struct Kcp;             // KCP控制块
pub struct KcpConfig;       // 配置
pub enum KcpCmd;            // 命令类型
pub struct KcpDeque;        // 双向队列
pub struct Segment;         // 数据段

// 错误处理
pub enum KcpError;
pub type KcpResult<T>;
```

## Stream API详解

### KcpStream - 客户端流

#### 创建连接

**基本用法**:
```rust
use kcp_ovo::KcpStream;

let stream = KcpStream::connect("127.0.0.1:8888")?;
```

**使用自定义配置**:
```rust
use kcp_ovo::{KcpStream, StreamConfig};
use std::time::Duration;

let config = StreamConfig {
    update_interval: Duration::from_millis(10),
    recv_buffer_size: 65536,
    auto_update: true,
    connect_timeout: Duration::from_secs(5),
};

let stream = KcpStream::connect_with_config("127.0.0.1:8888", config)?;
```

#### 发送数据

**方法1: write()**
```rust
use std::io::Write;

let data = b"Hello, KCP!";
let n = stream.write(data)?;
println!("发送了 {} 字节", n);
```

**方法2: write_all()**
```rust
use std::io::Write;

stream.write_all(b"Complete message")?;
```

**方法3: send()**
```rust
let n = stream.send(b"Data")?;
println!("发送了 {} 字节", n);
```

#### 接收数据

**方法1: read()**
```rust
use std::io::Read;

let mut buffer = [0u8; 1024];
let n = stream.read(&mut buffer)?;
println!("收到: {}", String::from_utf8_lossy(&buffer[..n]));
```

**方法2: recv()**
```rust
let mut buffer = [0u8; 1024];
let n = stream.recv(&mut buffer)?;
```

#### 查询状态

```rust
// 检查连接状态
if stream.is_connected() {
    println!("已连接");
}

// 获取远程地址
let remote = stream.remote_addr();
println!("远程地址: {}", remote);

// 获取本地地址
let local = stream.local_addr()?;
println!("本地地址: {}", local);
```

#### 关闭连接

```rust
stream.close()?;
```

### KcpListener - 服务端监听器

#### 创建监听器

**基本用法**:
```rust
use kcp_ovo::KcpListener;

let listener = KcpListener::bind("0.0.0.0:8888")?;
```

**使用自定义配置**:
```rust
let config = StreamConfig {
    update_interval: Duration::from_millis(10),
    recv_buffer_size: 65536,
    auto_update: true,
    connect_timeout: Duration::from_secs(5),
};

let listener = KcpListener::bind_with_config("0.0.0.0:8888", config)?;
```

#### 接受连接

**阻塞接受**:
```rust
let (stream, addr) = listener.accept()?;
println!("新连接来自: {}", addr);
```

**非阻塞接受**:
```rust
match listener.try_accept()? {
    Some((stream, addr)) => {
        println!("新连接来自: {}", addr);
        // 处理连接...
    }
    None => {
        println!("没有新连接");
    }
}
```

#### 查询状态

```rust
// 获取本地地址
let local = listener.local_addr()?;
println!("监听在: {}", local);
```

### StreamConfig - 流配置

```rust
pub struct StreamConfig {
    /// 自动更新间隔 (默认: 10ms)
    pub update_interval: Duration,

    /// 接收缓冲区大小 (默认: 65536)
    pub recv_buffer_size: usize,

    /// 是否自动调用update (默认: true)
    pub auto_update: bool,

    /// 连接超时时间 (默认: 5秒)
    pub connect_timeout: Duration,
}
```

**默认值**:
```rust
let config = StreamConfig {
    update_interval: Duration::from_millis(10),
    recv_buffer_size: 65536,
    auto_update: true,
    connect_timeout: Duration::from_secs(5),
};
```

## 底层API详解

### Kcp - KCP控制块

#### 创建实例

```rust
use kcp_ovo::{Kcp, KcpConfig};

// 使用默认配置
let kcp = Kcp::new(0x11223344, KcpConfig::default())?;

// 使用快速模式
let kcp = Kcp::new(0x11223344, KcpConfig::fast_mode())?;

// 使用自定义配置
let config = KcpConfig {
    mtu: 1400,
    interval: 100,
    nodelay: true,
    fastresend: 2,
    ..Default::default()
};
let kcp = Kcp::new(0x11223344, config)?;
```

**参数说明**:
- `conv`: 连接ID，必须与对端一致
- `config`: KCP配置

#### 设置回调

**输出回调** (必需):
```rust
kcp.set_output(move |data, _kcp| {
    socket.send(data)?;
    Ok(data.len())
});
```

**日志回调** (可选):
```rust
kcp.set_log(|msg, _kcp| {
    println!("{}", msg);
});
```

#### 发送数据

```rust
let data = b"Hello, KCP!";
let sent = kcp.send(data)?;
println!("发送了 {} 字节", sent);
```

**注意**: `send()`只是将数据放入发送队列，需要调用`flush()`才能真正发送。

#### 接收数据

```rust
let mut buffer = [0u8; 1024];
match kcp.recv(&mut buffer) {
    Ok(n) => {
        println!("收到: {}", String::from_utf8_lossy(&buffer[..n]));
    }
    Err(KcpError::QueueEmpty) => {
        println!("没有数据可接收");
    }
    Err(e) => {
        eprintln!("接收错误: {}", e);
    }
}
```

#### 输入数据

从UDP socket接收到数据包后，需要输入到KCP：

```rust
let mut udp_buffer = [0u8; 65536];
let (len, _src) = socket.recv_from(&mut udp_buffer)?;
kcp.input(&udp_buffer[..len])?;
```

#### 更新状态

定期调用`update()`来驱动KCP内部状态机：

```rust
use std::time::{SystemTime, UNIX_EPOCH};

// 获取当前时间戳(毫秒)
let current = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_millis() as u32;

kcp.update(current);
```

#### 查询下次更新时间

```rust
let next_update = kcp.check(current);
println!("下次更新时间: {}", next_update);
```

#### 查看队列状态

```rust
// 待发送数据量
let waitsnd = kcp.waitsnd();
println!("待发送: {} segments", waitsnd);

// 接收队列大小
let peek_size = kcp.peeksize()?;
println!("可接收数据大小: {}", peek_size);
```

### KcpConfig - 配置参数

```rust
pub struct KcpConfig {
    /// 最大传输单元 (默认: 1400)
    pub mtu: u32,

    /// 内部更新间隔(ms) (默认: 100)
    pub interval: u32,

    /// 是否启用无延迟模式 (默认: false)
    pub nodelay: bool,

    /// 快速重传触发次数 (默认: 0)
    pub fastresend: i32,

    /// 是否流式模式 (默认: false)
    pub stream: bool,

    /// 是否禁用拥塞控制 (默认: false)
    pub nocwnd: bool,

    /// 接收窗口大小 (默认: 128)
    pub rcv_wnd: u32,
}
```

#### 预定义配置

**默认配置**:
```rust
let config = KcpConfig::default();
// mtu: 1400
// interval: 100
// nodelay: false
// fastresend: 0
// stream: false
// nocwnd: false
// rcv_wnd: 128
```

**快速模式**:
```rust
let config = KcpConfig::fast_mode();
// mtu: 1400
// interval: 20
// nodelay: true
// fastresend: 2
// stream: false
// nocwnd: false
// rcv_wnd: 128
```

### KcpCmd - 命令类型

```rust
pub enum KcpCmd {
    Push,  // 数据推送命令
    Ack,   // 确认命令
    Wask,  // 窗口探测请求
    Wins,  // 窗口大小通知
}
```

### KcpError - 错误类型

```rust
pub enum KcpError {
    InvalidCommand(u32),   // 无效命令
    BufferTooSmall,        // 缓冲区太小
    QueueEmpty,            // 队列为空
    IncompleteData,        // 数据不完整
    InvalidSequence,       // 序列号错误
    InvalidConfig(String),  // 无效配置
    OutputNotSet,          // 输出回调未设置
    IoError(String),       // IO错误
}
```

## 配置参数

### 参数详解

#### mtu (最大传输单元)

- **类型**: `u32`
- **默认值**: `1400`
- **范围**: `[256, 65536]`
- **说明**: 控制单个数据包的最大大小
- **影响**:
  - 较大的MTU可以提高吞吐量
  - 较小的MTU可以减少丢包影响
  - MSS = MTU - 24 (KCP头部大小)

**推荐值**:
- 互联网: 1400 (安全)
- 内网: 9000 (jumbo frame)
- 高丢包网络: 500-800

#### interval (更新间隔)

- **类型**: `u32`
- **默认值**: `100` (ms)
- **范围**: `[10, 5000]`
- **说明**: KCP内部更新频率
- **影响**:
  - 较小的interval可以降低延迟
  - 较大的interval可以减少CPU使用

**推荐值**:
- 低延迟: 10-20 ms
- 平衡: 50-100 ms
- 省电: 200-500 ms

#### nodelay (无延迟模式)

- **类型**: `bool`
- **默认值**: `false`
- **说明**: 是否禁用Nagle算法
- **影响**:
  - 启用后立即发送小数据包
  - 禁用后会等待合并小包

**推荐值**:
- 游戏实时通信: true
- 文件传输: false

#### fastresend (快速重传)

- **类型**: `i32`
- **默认值**: `0`
- **范围**: `[0, 255]`
- **说明**: 触发快速重传的重复ACK数量
- **影响**:
  - 0 = 禁用快速重传
  - 2 = 收到2个重复ACK立即重传
  - 值越大越激进

**推荐值**:
- 高丢包网络: 2
- 正常网络: 0
- 实时应用: 2

#### nocwnd (禁用拥塞控制)

- **类型**: `bool`
- **默认值**: `false`
- **说明**: 是否禁用拥塞控制窗口
- **影响**:
  - 禁用后可以发送更多数据
  - 可能导致网络拥塞

**推荐值**:
- 内网: true
- 互联网: false
- 已知带宽: true

#### rcv_wnd (接收窗口)

- **类型**: `u32`
- **默认值**: `128`
- **范围**: `[32, 32768]`
- **说明**: 接收窗口大小(单位: segment)
- **影响**:
  - 较大的窗口可以提高吞吐量
  - 增加内存使用

**推荐值**:
- 低延迟: 128
- 高吞吐: 512-2048
- 内存受限: 32-64

### 配置模板

#### 游戏实时通信配置

```rust
let config = KcpConfig {
    mtu: 1400,
    interval: 10,        // 极快的更新
    nodelay: true,       // 立即发送
    fastresend: 2,       // 快速重传
    stream: false,
    nocwnd: false,       // 保留拥塞控制
    rcv_wnd: 256,        // 适中的窗口
};
```

#### 文件传输配置

```rust
let config = KcpConfig {
    mtu: 1400,
    interval: 100,       // 正常更新
    nodelay: false,      // 允许合并
    fastresend: 0,       // 超时重传
    stream: false,
    nocwnd: true,        // 禁用拥塞控制
    rcv_wnd: 2048,       // 大窗口
};
```

#### 视频流配置

```rust
let config = KcpConfig {
    mtu: 1400,
    interval: 50,        // 快速更新
    nodelay: true,       // 低延迟
    fastresend: 2,       // 快速重传
    stream: true,        // 流式模式
    nocwnd: false,
    rcv_wnd: 512,        // 大缓冲
};
```

## 错误处理

### 错误类型

```rust
use kcp_ovo::{KcpError, KcpResult};

fn send_data(kcp: &mut Kcp, data: &[u8]) -> KcpResult<usize> {
    kcp.send(data)
}
```

### 错误匹配

```rust
match kcp.send(data) {
    Ok(n) => println!("发送了 {} 字节", n),
    Err(KcpError::QueueEmpty) => {
        println!("队列为空");
    }
    Err(KcpError::BufferTooSmall) => {
        println!("缓冲区太小");
    }
    Err(KcpError::OutputNotSet) => {
        println!("需要先设置输出回调");
    }
    Err(e) => {
        eprintln!("未知错误: {}", e);
    }
}
```

### 错误转换

KCP错误自动从`std::io::Error`转换：

```rust
use std::io;
use kcp_ovo::KcpError;

// 自动转换
fn socket_operation() -> io::Result<usize> {
    Err(io::Error::new(io::ErrorKind::WouldBlock, "would block"))
}

// 可以使用?操作符
let result: KcpResult<usize> = socket_operation()?;
```

## 高级用法

### 多路复用

在单个UDP端口上运行多个KCP连接：

```rust
use std::collections::HashMap;
use kcp_ovo::Kcp;

struct KcpServer {
    connections: HashMap<u32, Kcp>,
    socket: UdpSocket,
}

impl KcpServer {
    fn handle_packet(&mut self, data: &[u8], addr: SocketAddr) -> KcpResult<()> {
        // 解析conv
        let conv = u32::from_be_bytes(data[0..4].try_into()?);

        // 查找或创建连接
        if !self.connections.contains_key(&conv) {
            let kcp = Kcp::new(conv, KcpConfig::default())?;
            self.connections.insert(conv, kcp);
        }

        // 输入数据
        if let Some(kcp) = self.connections.get_mut(&conv) {
            kcp.input(data)?;
        }

        Ok(())
    }
}
```

### 集成到Tokio

```rust
use tokio::net::UdpSocket;
use kcp_ovo::Kcp;

#[tokio::main]
async fn run_kcp() -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("0.0.0.0:8888").await?;
    let mut buf = [0u8; 65536];

    let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;
    kcp.set_output(|data, _| {
        // 异步发送
        let socket = socket.clone();
        tokio::spawn(async move {
            socket.send(data).await.ok();
        });
        Ok(data.len())
    });

    loop {
        let (len, _addr) = socket.recv_from(&mut buf).await?;
        kcp.input(&buf[..len])?;

        // 处理接收数据...
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}
```

### 自定义序列号管理

```rust
use std::sync::atomic::{AtomicU32, Ordering};

struct ConvGenerator {
    next_conv: AtomicU32,
}

impl ConvGenerator {
    fn new() -> Self {
        Self {
            next_conv: AtomicU32::new(1),
        }
    }

    fn generate(&self) -> u32 {
        self.next_conv.fetch_add(1, Ordering::SeqCst)
    }
}
```

### 性能监控

```rust
use std::time::Instant;

struct KcpMonitor {
    start_time: Instant,
    bytes_sent: u64,
    bytes_recv: u64,
}

impl KcpMonitor {
    fn new() -> Self {
        Self {
            start_time: Instant::now(),
            bytes_sent: 0,
            bytes_recv: 0,
        }
    }

    fn record_send(&mut self, n: usize) {
        self.bytes_sent += n as u64;
    }

    fn record_recv(&mut self, n: usize) {
        self.bytes_recv += n as u64;
    }

    fn throughput(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        (self.bytes_sent + self.bytes_recv) as f64 / elapsed
    }
}
```

## API速查表

### Stream API

| 操作 | 方法 | 说明 |
|------|------|------|
| 连接 | `KcpStream::connect()` | 连接到服务器 |
| 发送 | `write()` / `send()` | 发送数据 |
| 接收 | `read()` / `recv()` | 接收数据 |
| 关闭 | `close()` | 关闭连接 |
| 监听 | `KcpListener::bind()` | 绑定端口 |
| 接受 | `accept()` / `try_accept()` | 接受连接 |

### 底层API

| 操作 | 方法 | 说明 |
|------|------|------|
| 创建 | `Kcp::new()` | 创建KCP实例 |
| 发送 | `send()` | 发送数据到队列 |
| 接收 | `recv()` | 接收数据 |
| 输入 | `input()` | 输入UDP数据包 |
| 更新 | `update()` | 更新KCP状态 |
| 检查 | `check()` | 查询下次更新时间 |
| 查询 | `peeksize()` / `waitsnd()` | 查询状态 |
| 回调 | `set_output()` / `set_log()` | 设置回调 |

## 参考资源

- [KCP协议官方文档](https://github.com/skywind3000/kcp)
- [Rust标准库IO trait](https://doc.rust-lang.org/std/io/index.html)
- [示例程序](../examples/)
