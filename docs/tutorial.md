# KCP-OVO 入门教程

欢迎使用kcp-ovo！这是一个纯Rust实现的KCP协议库，提供低延迟、高可靠性的UDP传输。(注意维护时间，可能为旧版本)

## 目录

- [5分钟快速入门](#5分钟快速入门)
- [第一个KCP程序](#第一个kcp程序)
- [理解KCP基本概念](#理解kcp基本概念)
- [选择合适的API](#选择合适的api)
- [进阶主题](#进阶主题)

## 5分钟快速入门

### 安装

在您的`Cargo.toml`中添加：

```toml
[dependencies]
kcp-ovo = "0.1"
```

### Echo服务器

```rust
use kcp_ovo::KcpListener;
use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let mut listener = KcpListener::bind("0.0.0.0:8888")?;
    println!("服务器启动在 0.0.0.0:8888");

    let (mut stream, addr) = listener.accept()?;
    println!("客户端 {} 已连接", addr);

    let mut buffer = [0u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                stream.write_all(&buffer[..n])?;
            }
            Err(e) => {
                eprintln!("错误: {}", e);
                break;
            }
        }
    }

    Ok(())
}
```

### Echo客户端

```rust
use kcp_ovo::KcpStream;
use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let mut stream = KcpStream::connect("127.0.0.1:8888")?;

    stream.write_all(b"Hello, KCP!")?;
    println!("已发送: Hello, KCP!");

    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer)?;
    println!("收到: {}", String::from_utf8_lossy(&buffer[..n]));

    Ok(())
}
```

### 运行

```bash
# 终端1：启动服务器
cargo run --bin server

# 终端2：启动客户端
cargo run --bin client
```

## 第一个KCP程序

让我们创建一个简单的聊天室服务器和客户端。

### 服务器端 (examples/chat-server.rs)

```rust
use kcp_ovo::KcpListener;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::thread;

fn main() -> std::io::Result<()> {
    let mut listener = KcpListener::bind("0.0.0.0:9999")?;
    println!("聊天室服务器启动在 0.0.0.0:9999");

    let mut clients: Vec<KcpStream> = Vec::new();

    loop {
        let (mut stream, addr) = listener.accept()?;
        println!("新客户端: {}", addr);

        // 广播欢迎消息
        for client in &mut clients {
            let _ = client.write_all(format!("系统: 客户端 {} 加入\n", addr).as_bytes());
        }

        clients.push(stream);
    }
}
```

### 客户端 (examples/chat-client.rs)

```rust
use kcp_ovo::KcpStream;
use std::io::{self, Read, Write};
use std::thread;

fn main() -> io::Result<()> {
    let mut stream = KcpStream::connect("127.0.0.1:9999")?;

    // 启动接收线程
    let mut stream_clone = stream.try_clone()?;
    thread::spawn(move || {
        let mut buffer = [0u8; 1024];
        loop {
            match stream_clone.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    print!("{}", String::from_utf8_lossy(&buffer[..n]));
                }
                Err(_) => break,
            }
        }
    });

    // 主线程：读取用户输入并发送
    let mut input = String::new();
    let stdin = io::stdin();
    loop {
        input.clear();
        stdin.read_line(&mut input)?;
        stream.write_all(input.as_bytes())?;
    }
}
```

## 理解KCP基本概念

### 什么是KCP？

KCP（ARQ）是一个快速可靠协议，能以比TCP浪费10%-20%的带宽来换取更低的延迟。

### 主要特性

- **低延迟**: 相比TCP降低30%-40%的延迟
- **高可靠性**: 提供可靠传输保证
- **可配置**: 丰富的参数调整空间
- **简单**: 类似TCP的使用体验

### 核心参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `mtu` | 1400 | 最大传输单元 |
| `interval` | 100 | 内部更新间隔(ms) |
| `nodelay` | false | 是否启用无延迟模式 |
| `fastresend` | 0 | 快速重传触发次数 |
| `nocwnd` | false | 是否禁用拥塞控制 |
| `rcv_wnd` | 128 | 接收窗口大小 |

### 配置模式

**默认模式 (平衡模式)**:
```rust
let config = KcpConfig::default();
```

**快速模式 (低延迟)**:
```rust
let config = KcpConfig::fast_mode();
// 相当于：
// nodelay = true
// interval = 20
// fastresend = 2
```

**自定义配置**:
```rust
let config = KcpConfig {
    mtu: 1200,
    interval: 50,
    nodelay: true,
    fastresend: 2,
    ..Default::default()
};
```

## 选择合适的API

kcp-ovo提供两种API：**Stream API**和**底层API**。

### Stream API (推荐)

**适用场景**:
- ✅ 新项目
- ✅ 需要快速开发
- ✅ 类似TCP的使用体验
- ✅ 不需要精细控制

**优点**:
- 简单易用
- 自动管理KCP更新
- 实现标准trait (Read/Write)
- 自动处理socket

**示例**:
```rust
use kcp_ovo::KcpStream;
use std::io::{Read, Write};

// 连接
let mut stream = KcpStream::connect("127.0.0.1:8888")?;

// 使用标准IO操作
stream.write_all(b"Hello")?;
let mut buffer = [0u8; 1024];
stream.read(&mut buffer)?;
```

### 底层API

**适用场景**:
- ✅ 需要精细控制
- ✅ 集成到现有事件循环
- ✅ 需要零开销抽象
- ✅ 复杂的网络场景

**优点**:
- 完全控制KCP行为
- 精确管理update()时机
- 可以自定义输出
- 更灵活

**示例**:
```rust
use kcp_ovo::{Kcp, KcpConfig};

// 创建KCP实例
let mut kcp = Kcp::new(0x11223344, KcpConfig::fast_mode())?;

// 设置输出回调
kcp.set_output(|data, _kcp| {
    socket.send(data)?;
    Ok(data.len())
});

// 发送数据
kcp.send(b"Hello")?;

// 接收UDP数据包并输入到KCP
kcp.input(&udp_packet)?;

// 接收数据
let mut buffer = [0u8; 1024];
let len = kcp.recv(&mut buffer)?;

// 定期更新KCP状态
kcp.update(current_time_ms);
```

## 进阶主题

### 性能调优

#### 1. 低延迟场景 (游戏/实时通信)

```rust
let config = KcpConfig {
    nodelay: true,      // 启用无延迟
    interval: 10,       // 更快的更新频率
    fastresend: 2,      // 快速重传
    rcv_wnd: 512,       // 更大的接收窗口
    ..Default::default()
};
```

#### 2. 高吞吐场景 (文件传输)

```rust
let config = KcpConfig {
    nodelay: false,     // 禁用无延迟
    interval: 100,      // 正常更新频率
    fastresend: 0,      // 禁用快速重传
    rcv_wnd: 2048,      // 更大的窗口
    nocwnd: true,       // 禁用拥塞控制
    ..Default::default()
};
```

#### 3. 网络质量差 (高丢包/高延迟)

```rust
let config = KcpConfig {
    nodelay: false,     // 保守模式
    interval: 50,       // 适中的更新频率
    fastresend: 2,      // 启用快速重传
    rcv_wnd: 256,       // 适中的窗口
    ..Default::default()
};
```

### 错误处理

```rust
use kcp_ovo::KcpError;

match kcp.send(data) {
    Ok(n) => println!("发送了 {} 字节", n),
    Err(KcpError::QueueEmpty) => {
        println!("队列为空");
    }
    Err(KcpError::BufferTooSmall) => {
        println!("缓冲区太小");
    }
    Err(e) => {
        eprintln!("其他错误: {}", e);
    }
}
```

### 多线程安全

KCP本身不是线程安全的，需要在应用层进行同步：

```rust
use std::sync::{Arc, Mutex};

let kcp = Arc::new(Mutex::new(Kcp::new(conv, config)?));

// 线程1：发送
let kcp_clone = Arc::clone(&kcp);
thread::spawn(move || {
    loop {
        let mut kcp = kcp_clone.lock().unwrap();
        // 发送操作...
    }
});

// 线程2：接收
let kcp_clone = Arc::clone(&kcp);
thread::spawn(move || {
    loop {
        let mut kcp = kcp_clone.lock().unwrap();
        // 接收操作...
    }
});
```

## 常见问题

### Q: 如何生成唯一的conv ID？

A: 简单场景可以使用时间戳，生产环境建议通过握手协商：

```rust
use std::time::{SystemTime, UNIX_EPOCH};

let conv = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_secs() as u32;
```

### Q: 如何处理连接超时？

A: Stream API示例：

```rust
let config = StreamConfig {
    connect_timeout: Duration::from_secs(5),
    ..Default::default()
};

let stream = KcpStream::connect_with_config("127.0.0.1:8888", config)?;
```

### Q: 如何获取RTT和RTO？

A: 通过底层API访问：

```rust
println!("RTT: {} ms", kcp.rx_srtt);
println!("RTO: {} ms", kcp.rx_rto);
```

## 下一步

- 查看 [API指南](api-guide.md) 了解详细API
- 阅读 [性能优化](performance.md) 学习调优技巧
- 参考 [故障排查](troubleshooting.md) 解决问题
- 运行 `examples/` 目录下的示例程序

## 示例程序

项目包含以下示例：

| 示例 | 说明 | 运行方式 |
|------|------|---------|
| `stream-api.rs` | Echo服务器/客户端 | `cargo run --example stream-api -- server` |
| `low-level-api.rs` | 底层API演示 | `cargo run --example low-level-api` |

更多示例请查看 `examples/` 目录。

## 获取帮助

- GitHub Issues: [kcp-ovo/issues](https://github.com/cherish-ltt/kcp-ovo/issues)
- 文档: `cargo doc --open`
