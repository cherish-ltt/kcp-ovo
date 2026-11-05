//! Criterion 真实耗时基准：KCP 在不同丢包率下完成一次传输的处理成本。
//!
//! 测量模型：
//! - 每个 benchmark 迭代新建一对 `Kcp`，从零开始把 `SCENARIOS` 指定的数据量送达，
//!   因此测量值包含建立连接与数据交付的全过程，迭代之间互不影响
//! - 协议使用虚拟时钟且不 sleep；Criterion 只测真实耗时，不混入虚拟时间。
//! - 每轮使用相同输入和种子，避免迭代数改变丢包序列；覆盖范围不是随机压力测试。
//! - 数据逐消息校验，超时、协议错误或未确认完毕均失败，不计作成功样本。
//! - 包含连接创建、仿真链路、校验和析构开销，不等于纯 CPU 周期或真实 UDP 吞吐。
//!
//! 运行：
//! ```bash
//! cargo bench --bench kcp-transfer -- --save-baseline before-core-perf
//! cargo bench --bench kcp-transfer -- --baseline before-core-perf
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kcp_ovo::config::KcpConfig;
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::error::{IncompleteDataType, KcpError};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

/// 固定窗口配置，使前后两次测量使用相同的队列规模。
const BENCH_SND_WND: u16 = 512;
/// 接收窗口需不小于发送窗口，否则对方的窗口通告会先成为瓶颈。
const BENCH_RCV_WND: u16 = 512;

/// 各档丢包率对应的单次传输数据量（丢包率, 字节数）。
///
/// 保留既有数据量；只比较同一场景的前后结果，不横向比较不同丢包档位。
const SCENARIOS: [(f64, usize); 5] =
    [(0.0, 4 * 1024 * 1024), (0.2, 12 * 1024), (0.4, 2 * 1024), (0.6, 160), (0.8, 32)];
/// 单次迭代允许推进的最大虚拟毫秒数，防止极端丢包下无限循环。
const MAX_VIRTUAL_MS: u64 = 600_000;
/// 时钟步进，比协议刷新间隔更细以覆盖重传期限。
const TICK_MS: u64 = 1;
const CONV: u32 = 0x1122_3344;
const SEED: u64 = 0x4b43_505f_4245_4e43;

/// 单向链路：按概率丢包，存活包放入 inflight 交给对端
struct Link {
    loss: f64,
    rng: StdRng,
    inflight: VecDeque<Vec<u8>>,
}

impl Link {
    fn new(loss: f64, seed: u64) -> Self {
        Self { loss, rng: StdRng::seed_from_u64(seed), inflight: VecDeque::new() }
    }

    fn admit(&mut self, packets: &Arc<Mutex<VecDeque<Vec<u8>>>>) {
        let mut packets = packets.lock().unwrap();
        for p in packets.drain(..) {
            if self.loss > 0.0 && self.rng.random::<f64>() < self.loss {
                continue;
            }
            self.inflight.push_back(p);
        }
    }

    fn deliver(&mut self, peer: &mut Kcp) {
        while let Some(p) = self.inflight.pop_front() {
            peer.input(&p).expect("valid simulated packet");
        }
    }
}

/// 返回虚拟推进量供 black_box 消费；它不参与 Criterion 的计时。
async fn transfer_once(loss: f64, payload: &[u8]) -> u64 {
    let cfg = KcpConfig::fast_mode_with_wnd(BENCH_SND_WND, BENCH_RCV_WND);
    let mut sender = Kcp::new(CONV, cfg.clone()).unwrap();
    let mut receiver = Kcp::new(CONV, cfg.clone()).unwrap();

    let up_out: Arc<Mutex<VecDeque<Vec<u8>>>> = Arc::new(Mutex::new(VecDeque::new()));
    let down_out: Arc<Mutex<VecDeque<Vec<u8>>>> = Arc::new(Mutex::new(VecDeque::new()));

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

    let mut up = Link::new(loss, SEED);
    let mut down = Link::new(loss, SEED ^ 0x9e37_79b9);

    let mut sent = 0usize;
    let mut recv_bytes = 0usize;
    let mut now = 0;

    while now < MAX_VIRTUAL_MS {
        while sent < payload.len() && sender.waitsnd() < 1024 {
            let size = (payload.len() - sent).min(cfg.mtu as usize);
            sender.send(&payload[sent..sent + size]).unwrap();
            sent += size;
        }

        sender.update(now).await.unwrap();
        receiver.update(now).await.unwrap();

        up.admit(&up_out);
        up.deliver(&mut receiver);
        loop {
            match receiver.recv() {
                Ok(data) => {
                    let size = (payload.len() - recv_bytes).min(cfg.mtu as usize);
                    assert_eq!(data.as_slice(), &payload[recv_bytes..recv_bytes + size]);
                    recv_bytes += data.len();
                },
                Err(KcpError::QueueEmpty)
                | Err(KcpError::IncompleteData(IncompleteDataType::PayloadWaitForFrg)) => break,
                Err(error) => panic!("receive failed: {error}"),
            }
        }

        down.admit(&down_out);
        down.deliver(&mut sender);

        if recv_bytes == payload.len() && sender.waitsnd() == 0 {
            return now;
        }
        now += TICK_MS;
    }

    panic!(
        "transfer timed out: loss={loss}, received={recv_bytes}/{}, pending={}",
        payload.len(),
        sender.waitsnd()
    );
}

fn bench_kcp_transfer(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread().build().expect("build tokio runtime");

    let mut group = c.benchmark_group("kcp_transfer_wall");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    for (loss, payload) in SCENARIOS {
        group.throughput(Throughput::Bytes(payload as u64));
        let payload: Vec<u8> =
            (0..payload).map(|i| (i.wrapping_mul(31) ^ (i >> 8)) as u8).collect();
        let id = format!("loss_{:.0}", loss * 100.0);
        group.bench_function(&id, |b| {
            b.to_async(&rt).iter(|| transfer_once(loss, &payload));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_kcp_transfer);
criterion_main!(benches);
