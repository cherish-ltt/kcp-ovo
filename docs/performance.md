# KCP-OVO 性能优化指南

本指南介绍如何优化kcp-ovo的性能，以适应不同的使用场景。

## 目录

- [性能基准](#性能基准)
- [延迟优化](#延迟优化)
- [吞吐量优化](#吞吐量优化)
- [网络适配](#网络适配)
- [内存优化](#内存优化)
- [CPU优化](#cpu优化)
- [调优工具](#调优工具)

## 性能基准

### KCP vs TCP

| 指标 | TCP | KCP (默认) | KCP (快速模式) |
|------|-----|-----------|---------------|
| 延迟 | 基准 | -30% ~ -40% | -50% ~ -60% |
| 带宽利用率 | 100% | 90-95% | 85-90% |
| CPU使用 | 低 | 中 | 中高 |

### 典型场景性能

**游戏实时通信** (KCP快速模式):
- 延迟: 20-50ms (vs TCP 80-150ms)
- 丢包恢复: < 100ms
- 适合: FPS游戏、实时对战

**视频直播** (KCP平衡配置):
- 延迟: 100-200ms
- 吞吐量: 2-5 Mbps
- 适合: 720p直播

**文件传输** (KCP高吞吐配置):
- 延迟: 200-500ms
- 吞吐量: 10-50 Mbps
- 适合: 大文件下载

## 延迟优化

### 1. 启用无延迟模式

```rust
let config = KcpConfig {
    nodelay: true,        // 立即发送，不等待
    interval: 10,         // 最快更新频率
    fastresend: 2,        // 快速重传
    ..Default::default()
};
```

**效果**: 降低50-60%延迟
**代价**: 增加10-20%带宽使用

### 2. 减小更新间隔

```rust
let config = KcpConfig {
    interval: 10,         // 从100降至10ms
    ..Default::default()
};
```

**效果**: 降低20-30%延迟
**代价**: 增加10倍CPU使用

**推荐值**:
- 实时游戏: 10-20ms
- 视频通话: 20-50ms
- 普通应用: 50-100ms

### 3. 启用快速重传

```rust
let config = KcpConfig {
    fastresend: 2,        // 收到2个重复ACK立即重传
    ..Default::default()
};
```

**效果**: 降低30-50%丢包恢复时间
**代价**: 可能增加网络负载

### 4. 调整窗口大小

```rust
let config = KcpConfig {
    rcv_wnd: 256,         // 增大接收窗口
    ..Default::default()
};
```

**效果**: 提高吞吐，间接降低延迟
**代价**: 增加内存使用

## 吞吐量优化

### 1. 禁用拥塞控制

```rust
let config = KcpConfig {
    nocwnd: true,         // 禁用拥塞窗口限制
    ..Default::default()
};
```

**适用场景**:
- 内网传输
- 已知可用带宽
- 专用网络

**效果**: 提高2-3倍吞吐量
**风险**: 可能导致网络拥塞

### 2. 增大接收窗口

```rust
let config = KcpConfig {
    rcv_wnd: 2048,        // 从128增至2048
    ..Default::default()
};
```

**效果**: 提高10-15倍吞吐量
**代价**: 内存使用增加16倍

**推荐值**:
- 低延迟场景: 128-256
- 高吞吐场景: 512-2048
- 内存受限: 32-64

### 3. 调整MTU大小

```rust
let config = KcpConfig {
    mtu: 9000,            // Jumbo Frame
    ..Default::default()
};
```

**网络环境**:
- 互联网: 1400 (安全值)
- 内网(千兆): 9000 (Jumbo Frame)
- 内网(万兆): 64000 (最大)

**效果**: 提高20-30%吞吐量
**要求**: 网络路径支持大包

### 4. 禁用快速重传

```rust
let config = KcpConfig {
    fastresend: 0,        // 禁用
    nodelay: false,       // 允许合并
    interval: 100,        // 正常更新
    ..Default::default()
};
```

**效果**: 提高10-20%带宽利用率
**适用**: 批量传输、文件下载

## 网络适配

### 高丢包网络 (>10%)

```rust
let config = KcpConfig {
    nodelay: false,       // 保守模式
    interval: 50,         // 快速更新
    fastresend: 2,        // 快速重传
    rcv_wnd: 128,         // 适中窗口
    ..Default::default()
};
```

### 高延迟网络 (>200ms RTT)

```rust
let config = KcpConfig {
    nodelay: false,       // 禁用
    interval: 100,        // 正常更新
    fastresend: 0,        // 超时重传
    rcv_wnd: 512,         // 大窗口
    ..Default::default()
};
```

### 低带宽 (<1Mbps)

```rust
let config = KcpConfig {
    nodelay: false,       // 合并小包
    interval: 200,        // 降低更新频率
    fastresend: 0,
    rcv_wnd: 64,          // 小窗口
    ..Default::default()
};
```

### 高带宽 (>100Mbps)

```rust
let config = KcpConfig {
    mtu: 9000,            // 大包
    nodelay: true,        // 低延迟
    interval: 20,
    fastresend: 2,
    rcv_wnd: 2048,        // 大窗口
    nocwnd: true,         // 禁用拥塞控制
    ..Default::default()
};
```

## 内存优化

### 1. 调整缓冲区大小

**Stream API**:
```rust
let config = StreamConfig {
    recv_buffer_size: 16384,  // 从65536降至16KB
    ..Default::default()
};
```

**底层API**:
```rust
let config = KcpConfig {
    rcv_wnd: 32,          // 从128降至32
    ..Default::default()
};
```

**内存计算**:
- 每个segment = MTU大小 (约1400字节)
- rcv_wnd = 32 → 约45KB
- rcv_wnd = 2048 → 约2.8MB

### 2. 重用缓冲区

```rust
struct KcpConnection {
    kcp: Kcp,
    recv_buffer: Vec<u8>,  // 重用
    send_buffer: Vec<u8>,  // 重用
}

impl KcpConnection {
    fn new(conv: u32) -> Self {
        Self {
            kcp: Kcp::new(conv, KcpConfig::default()).unwrap(),
            recv_buffer: vec![0u8; 8192],
            send_buffer: vec![0u8; 8192],
        }
    }

    fn recv_data(&mut self) -> KcpResult<&[u8]> {
        let len = self.kcp.recv(&mut self.recv_buffer)?;
        Ok(&self.recv_buffer[..len])
    }
}
```

### 3. 使用池化

```rust
use object_pool::Pool;

struct BufferPool {
    pool: Pool<Vec<u8>>,
}

impl BufferPool {
    fn new() -> Self {
        Self {
            pool: Pool::new(100, || vec![0u8; 8192]),
        }
    }

    fn get(&self) -> Vec<u8> {
        self.pool.pull()
    }
}
```

## CPU优化

### 1. 降低更新频率

```rust
let config = KcpConfig {
    interval: 100,        // 从10ms增至100ms
    ..Default::default()
};
```

**效果**: 降低90% CPU使用
**代价**: 增加10-20ms延迟

### 2. 批量处理

```rust
impl KcpConnection {
    fn process_batch(&mut self, packets: &[&[u8]]) -> KcpResult<()> {
        // 批量输入
        for packet in packets {
            self.kcp.input(*packet)?;
        }

        // 一次性更新
        self.kcp.update(get_current_ms());

        // 批量发送
        self.kcp.flush();

        Ok(())
    }
}
```

### 3. 异步处理

```rust
use tokio::task::JoinHandle;

struct AsyncKcp {
    kcp: Arc<Mutex<Kcp>>,
    update_task: Option<JoinHandle<()>>,
}

impl AsyncKcp {
    fn start_update_loop(&mut self) {
        let kcp = self.kcp.clone();
        self.update_task = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mut kcp = kcp.lock().unwrap();
                kcp.update(get_current_ms());
            }
        }));
    }
}
```

## 调优工具

### 1. 性能监控

```rust
struct KcpMetrics {
    bytes_sent: AtomicU64,
    bytes_recv: AtomicU64,
    packets_sent: AtomicU64,
    packets_recv: AtomicU64,
    retransmissions: AtomicU64,
}

impl KcpMetrics {
    fn record_send(&self, n: usize) {
        self.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
    }

    fn get_loss_rate(&self) -> f64 {
        let sent = self.packets_sent.load(Ordering::Relaxed);
        let retrans = self.retransmissions.load(Ordering::Relaxed);
        if sent > 0 {
            (retrans as f64) / (sent as f64)
        } else {
            0.0
        }
    }

    fn get_throughput(&self, elapsed_secs: f64) -> f64 {
        let total = self.bytes_sent.load(Ordering::Relaxed) +
                   self.bytes_recv.load(Ordering::Relaxed);
        total as f64 / elapsed_secs
    }
}
```

### 2. 自适应调优

```rust
struct AdaptiveKcp {
    kcp: Kcp,
    metrics: KcpMetrics,
    last_loss_rate: f64,
}

impl AdaptiveKcp {
    fn adjust_params(&mut self) {
        let loss_rate = self.metrics.get_loss_rate();

        if loss_rate > 0.1 {
            // 高丢包：降低窗口
            self.kcp.rcv_wnd = self.kcp.rcv_wnd.saturating_sub(16);
        } else if loss_rate < 0.01 {
            // 低丢包：增加窗口
            self.kcp.rcv_wnd = self.kcp.rcv_wnd.saturating_add(16);
        }

        self.last_loss_rate = loss_rate;
    }
}
```

### 3. 基准测试

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_kcp_send(c: &mut Criterion) {
    let mut kcp = Kcp::new(0x11223344, KcpConfig::default()).unwrap();
    kcp.set_output(|_, _| Ok(100));

    c.bench_function("kcp_send_1kb", |b| {
        b.iter(|| {
            let data = vec![0u8; 1024];
            kcp.send(black_box(&data)).ok();
            kcp.flush();
        });
    });
}

criterion_group!(benches, bench_kcp_send);
criterion_main!(benches);
```

## 配置检查表

### 游戏实时通信 ✅

- [ ] `nodelay = true`
- [ ] `interval = 10-20`
- [ ] `fastresend = 2`
- [ ] `rcv_wnd = 128-256`
- [ ] `nocwnd = false`

### 视频直播 ✅

- [ ] `nodelay = true`
- [ ] `interval = 20-50`
- [ ] `fastresend = 2`
- [ ] `rcv_wnd = 512-1024`
- [ ] `stream = true`

### 文件传输 ✅

- [ ] `nodelay = false`
- [ ] `interval = 100-200`
- [ ] `fastresend = 0`
- [ ] `rcv_wnd = 2048`
- [ ] `nocwnd = true`

### IoT设备 ✅

- [ ] `nodelay = false`
- [ ] `interval = 100-500`
- [ ] `fastresend = 0`
- [ ] `rcv_wnd = 32-64`
- [ ] 小缓冲区

## 常见性能问题

### 问题1: 高延迟

**症状**: 延迟> 200ms

**诊断**:
```rust
println!("RTT: {} ms", kcp.rx_srtt);
println!("RTO: {} ms", kcp.rx_rto);
println!("SND queue: {}", kcp.waitsnd());
```

**解决方案**:
1. 启用`nodelay`
2. 减小`interval`
3. 启用`fastresend`

### 问题2: 低吞吐量

**症状**: 吞吐量 < 1Mbps

**诊断**:
```rust
println!("CWND: {}", kcp.cwnd);
println!("RMT window: {}", kcp.rmt_wnd);
println!("SND queue: {}", kcp.waitsnd());
```

**解决方案**:
1. 增大`rcv_wnd`
2. 禁用`nocwnd`
3. 增大`mtu`

### 问题3: 高丢包率

**症状**: 丢包率 > 5%

**诊断**:
```rust
let loss_rate = retransmissions as f64 / total_packets as f64;
println!("Loss rate: {:.2}%", loss_rate * 100.0);
```

**解决方案**:
1. 检查网络质量
2. 减小`mtu`
3. 减小`rcv_wnd`

### 问题4: 高CPU使用

**症状**: CPU > 50%

**诊断**:
```rust
println!("Update interval: {}", kcp.interval);
println!("SND queue: {}", kcp.waitsnd());
```

**解决方案**:
1. 增大`interval`
2. 批量处理
3. 异步更新

## 最佳实践

### 1. 渐进式调优

```rust
// 从保守配置开始
let mut config = KcpConfig {
    interval: 100,
    nodelay: false,
    fastresend: 0,
    rcv_wnd: 128,
    ..Default::default()
};

// 根据网络状况调整
if rtt < 50 {
    config.interval = 20;
    config.nodelay = true;
}

if loss_rate < 0.01 {
    config.fastresend = 2;
    config.rcv_wnd = 256;
}
```

### 2. 监控和日志

```rust
kcp.set_log(|msg, kcp| {
    println!("[KCP] {} | snd_q={} rtt={}",
        msg, kcp.waitsnd(), kcp.rx_srtt);
});
```

### 3. 性能测试

```bash
# 基准测试
cargo bench

# 性能分析
cargo flamegraph --example stream-api

# 内存分析
valgrind --tool=massif cargo run --example stream-api
```

## 参考资源

- [KCP协议性能分析](https://github.com/skywind3000/kcp/blob/master/README.en.md)
- [网络调优指南](https://www.kernel.org/doc/Documentation/networking/ip-sysctl.txt)
- [Rust性能优化](https://nnethercote.github.io/perf-book/)
