//! criterion 标准基准：KCP 在不同丢包率下完成一次传输所需的虚拟时长
//!
//! 测量模型：
//! - 每个 benchmark 迭代新建一对 `Kcp`，从零开始把 `SCENARIOS` 指定的数据量送达，
//!   因此测量值包含建立连接与数据交付的全过程，迭代之间互不影响
//! - 时钟由虚拟毫秒驱动，按 tick 递增推进 `update()`，不做真实 sleep；
//!   测量值是虚拟推进量（协议视角的传输时长），不是 CPU 跑模拟的真实耗时
//! - 丢包在 output 回调处按概率丢弃，数据包与 ACK 都丢，种子随迭代递增以覆盖更多丢包序列
//! - 每档丢包率的数据量经过标定，使 0%~60% 的单次虚拟时长落在数百毫秒到一秒量级，
//!   便于人眼直接对比；80% 档受重传退避限制无法压到该区间，详见 `SCENARIOS` 说明
//!
//! 运行：
//! ```bash
//! cargo bench
//! cargo bench -- loss_80
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kcp_ovo::config::KcpConfig;
use kcp_ovo::core::kcp::Kcp;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

/// 各档丢包率对应的单次传输数据量（丢包率, 字节数）。
///
/// 数据量按实测标定，使各组的单次虚拟时长落在数百毫秒到一秒量级，
/// 便于人眼直接对比：丢包越重，同样时长能送出的数据越少。
///
/// 注意 80% 档无法靠缩小数据量把时长压下来：即使只传 1 字节也需要约 11 秒，
/// 这是该丢包率下重传退避决定的固定下限，属于协议行为的真实反映。
const SCENARIOS: [(f64, usize); 5] = [
    (0.0, 320 * 1024),
    (0.2, 12 * 1024),
    (0.4, 2 * 1024),
    (0.6, 160),
    (0.8, 32),
];
/// 单次迭代允许推进的最大虚拟毫秒数，防止极端丢包下无限循环。
const MAX_VIRTUAL_MS: u64 = 60_000;
/// 时钟步进，与 `KcpConfig::fast_mode()` 的 interval 同量级。
const TICK_MS: u64 = 1;
const CONV: u32 = 0x1122_3344;
const SEED: u64 = 0x4b43_505f_4245_4e43;

/// 单向链路：按概率丢包，存活包放入 inflight 交给对端
struct Link {
    loss: f64,
    rng: StdRng,
    inflight: VecDeque<Bytes>,
}

impl Link {
    fn new(loss: f64, seed: u64) -> Self {
        Self {
            loss,
            rng: StdRng::seed_from_u64(seed),
            inflight: VecDeque::new(),
        }
    }

    fn admit(&mut self, packets: &Arc<Mutex<VecDeque<Bytes>>>) {
        let drained: Vec<Bytes> = packets.lock().unwrap().drain(..).collect();
        for p in drained {
            if self.loss > 0.0 && self.rng.random::<f64>() < self.loss {
                continue;
            }
            self.inflight.push_back(p);
        }
    }

    fn deliver(&mut self, peer: &mut Kcp) {
        while let Some(p) = self.inflight.pop_front() {
            let _ = peer.input(&p);
        }
    }
}

/// 一次性完成 `loss` 丢包率下的完整传输，返回消耗的虚拟毫秒数。
///
/// 返回虚拟时间而不是真实耗时，评判交给 criterion 的统计；
/// 一轮的结果用于防止极端丢包下跑不完。
async fn transfer_once(loss: f64, seed: u64, payload: usize) -> u64 {
    let cfg = KcpConfig::fast_mode();
    let mut sender = Kcp::new(CONV, cfg.clone()).unwrap();
    let mut receiver = Kcp::new(CONV, cfg.clone()).unwrap();

    let up_out: Arc<Mutex<VecDeque<Bytes>>> = Arc::new(Mutex::new(VecDeque::new()));
    let down_out: Arc<Mutex<VecDeque<Bytes>>> = Arc::new(Mutex::new(VecDeque::new()));

    let sink = up_out.clone();
    sender.set_output(move |data| {
        let sink = sink.clone();
        async move {
            let len = data.len();
            sink.lock().unwrap().push_back(data);
            Ok(len)
        }
    });
    let sink = down_out.clone();
    receiver.set_output(move |data| {
        let sink = sink.clone();
        async move {
            let len = data.len();
            sink.lock().unwrap().push_back(data);
            Ok(len)
        }
    });

    let mut up = Link::new(loss, seed);
    let mut down = Link::new(loss, seed ^ 0x9e37_79b9);

    let chunk = vec![0x5au8; cfg.mtu as usize];
    let mut sent = 0usize;
    let mut recv_bytes = 0usize;
    // 从中段起步，让 header 中由真实时钟生成的 ts 与虚拟时钟处于同一量级
    let mut now = kcp_ovo::helper::current_millis();
    let start_ms = now;

    while recv_bytes < payload && now - start_ms < MAX_VIRTUAL_MS {
        while sent < payload && sender.waitsnd() < 1024 {
            let size = (payload - sent).min(chunk.len());
            sender.send(&chunk[..size]).unwrap();
            sent += size;
        }

        sender.update(now).await.unwrap();
        receiver.update(now).await.unwrap();

        up.admit(&up_out);
        up.deliver(&mut receiver);
        while let Ok(data) = receiver.recv() {
            recv_bytes += data.len();
        }

        down.admit(&down_out);
        down.deliver(&mut sender);

        now += TICK_MS;
    }

    now - start_ms
}

fn bench_kcp_transfer(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build tokio runtime");

    let mut group = c.benchmark_group("kcp_transfer");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    for (loss, payload) in SCENARIOS {
        group.throughput(Throughput::Bytes(payload as u64));
        let id = format!("loss_{:.0}", loss * 100.0);
        group.bench_function(&id, |b| {
            // 测量值是协议视角的传输时长（虚拟毫秒推进量），不是 CPU 跑模拟的耗时，
            // 后者只反映算力、与网络质量无关。criterion 会把 iter_custom 的返回值
            // 直接当作测量值且不做分摊，故这里返回整批 iters 次的虚拟毫秒总量。
            //
            // 叠加真实耗时是为了给 criterion 提供方差：虚拟时长完全确定，零方差的
            // 输入会让它的 KDE 估计直接 panic。真实耗时只占虚拟时长的千分之一量级，
            // 不改变各组之间的量级关系。
            b.to_async(&rt).iter_custom(|iters| async move {
                let wall = std::time::Instant::now();
                let mut total_ms = 0u64;
                for i in 0..iters {
                    total_ms += transfer_once(loss, SEED.wrapping_add(i), payload).await;
                }
                Duration::from_millis(total_ms) + wall.elapsed()
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_kcp_transfer);
criterion_main!(benches);
