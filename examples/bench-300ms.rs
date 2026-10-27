//! 300ms 固定窗口吞吐基准
//!
//! 度量两种口径：
//! - `cold`：从新建 Kcp 就开始计时，300ms 内送达字节数（含握手与窗口探测开销）
//! - `warm`：先由应用层补一个窗口通告包打开 rmt_wnd，再开始计时（稳态吞吐）
//!
//! 实现要点：
//! - 收发两端同进程内存直连，无真实 socket 与 syscall 噪声
//! - 丢包在 output 回调处按概率丢弃，数据包与 ACK 都丢，种子固定可复现
//! - 主指标是接收端 `recv()` 实际取出的字节数，不是发送端入队字节数
//! - 每个丢包率跑 `ROUNDS` 轮，各轮换种子，取中位数以压制调度噪声
//! - 附带 50/100/200/300ms 曲线与全部送达耗时，便于观察优化前后的形状变化
//!
//! 注意：当前实现两端在 rmt_wnd=0 时都不发包，都要等 IKCP_PROBE_INIT(7s) 的窗口探测，
//! 因此 `cold` 在 300ms 内几乎必然为 0，这是协议行为而不是 bench 误差。
//!
//! 运行：
//! ```bash
//! cargo run --release --example bench-300ms
//! cargo run --release --example bench-300ms -- --total 1048576 --rounds 5 --seed 7
//! ```

use std::collections::VecDeque;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use kcp_ovo::config::KcpConfig;
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::{KcpCmd, KcpPacket, KcpPacketHeader, current_millis};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

const WINDOW_MS: u64 = 300;
const CURVE_MS: [u64; 4] = [50, 100, 200, 300];
const LOSSES: [f64; 5] = [0.0, 0.2, 0.4, 0.6, 0.8];
const DEFAULT_TOTAL: usize = 1024 * 1024;
const DEFAULT_ROUNDS: usize = 3;
const TICK_MS: u64 = 1;
const DEADLINE_MS: u64 = 5_000;
const DEFAULT_SEED: u64 = 0x4b43_505f_4245_4e43;
const CONV: u32 = 0x1122_3344;

/// 单向链路：按概率丢包，存活包放入 inflight 交给对端
struct Link {
    loss: f64,
    rng: StdRng,
    inflight: VecDeque<Bytes>,
    passed: u64,
    dropped: u64,
}

impl Link {
    fn new(loss: f64, seed: u64) -> Self {
        Self {
            loss,
            rng: StdRng::seed_from_u64(seed),
            inflight: VecDeque::new(),
            passed: 0,
            dropped: 0,
        }
    }

    /// 把 output 产生的包按丢包率过滤后放入 inflight
    fn admit(&mut self, packets: &Arc<Mutex<VecDeque<Bytes>>>) {
        let drained: Vec<Bytes> = packets.lock().unwrap().drain(..).collect();
        for p in drained {
            if self.loss > 0.0 && self.rng.random::<f64>() < self.loss {
                self.dropped += 1;
            } else {
                self.passed += 1;
                self.inflight.push_back(p);
            }
        }
    }

    fn deliver(&mut self, peer: &mut Kcp) {
        while let Some(p) = self.inflight.pop_front() {
            peer.input(&p).unwrap();
        }
    }
}

struct Pair {
    sender: Kcp,
    receiver: Kcp,
    up: Link,
    down: Link,
    up_out: Arc<Mutex<VecDeque<Bytes>>>,
    down_out: Arc<Mutex<VecDeque<Bytes>>>,
}

impl Pair {
    fn new(cfg: &KcpConfig, loss: f64, seed: u64) -> Self {
        let mut sender = Kcp::new(CONV, cfg.clone()).unwrap();
        let mut receiver = Kcp::new(CONV, cfg.clone()).unwrap();

        // output 回调是 Fn + 'static，只能把包推入共享队列，丢包判定在主循环里做
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

        Self {
            sender,
            receiver,
            up: Link::new(loss, seed),
            down: Link::new(loss, seed ^ 0x9e37_79b9),
            up_out,
            down_out,
        }
    }

    /// 单 tick：双端 update → 上行投递 → 接收端取出 → 下行投递
    /// 返回本 tick 从接收端取出的字节数
    async fn step(&mut self, now_ms: u64) -> usize {
        self.sender.update(now_ms).await.unwrap();
        self.receiver.update(now_ms).await.unwrap();

        self.up.admit(&self.up_out);
        self.up.deliver(&mut self.receiver);

        let mut got = 0;
        while let Ok(data) = self.receiver.recv() {
            got += data.len();
        }

        self.down.admit(&self.down_out);
        self.down.deliver(&mut self.sender);
        got
    }

    fn stats(&self) -> (u64, u64) {
        (
            self.up.passed + self.down.passed,
            self.up.dropped + self.down.dropped,
        )
    }
}

struct Result {
    at_window: usize,
    curve: Vec<(u64, usize)>,
    total_ms: Option<u64>,
    passed: u64,
    dropped: u64,
    warmed: bool,
    /// 在窗口结束前就跑完了 total，说明该窗口值被 total 截断，不代表真实吞吐
    saturated: bool,
}

async fn run(cfg: &KcpConfig, total: usize, loss: f64, seed: u64, warm: bool) -> Result {
    let mut pair = Pair::new(cfg, loss, seed);
    let chunk = vec![0x5au8; cfg.mtu as usize];
    let base_ms = current_millis();
    let now = base_ms;

    // 预热：当前实现里两端在 rmt_wnd=0 时都不发包，都在等 IKCP_PROBE_INIT(7s) 的窗口探测，
    // 不存在任何一方主动送出的首包。这里由应用层补一个 Wins 包通告接收端窗口，
    // 把发送端的 rmt_wnd 打开，之后再计时得到的才是稳态吞吐，而不是协议在死等。
    let mut warmed = false;
    if warm {
        let wins = KcpPacket::new_without_data(KcpPacketHeader::new(
            CONV,
            KcpCmd::Wins,
            0,
            cfg.rcv_wnd,
            current_millis(),
            0,
            0,
            0,
        ))
        .to_bytes()
        .unwrap();
        pair.sender.input(&wins).unwrap();
        warmed = pair.sender.rmt_wnd > 0;
    }

    let mut sent = 0usize;
    let mut recv_bytes = 0usize;
    let mut curve: Vec<(u64, usize)> = Vec::new();
    let mut at_window = 0usize;
    let mut total_ms = None;
    let mut next_curve = 0usize;
    let start = Instant::now();
    let start_ms = now;

    loop {
        let elapsed = start.elapsed().as_millis() as u64;

        // 灌数据：只受 KCP 自身窗口约束，不做应用层限速
        while sent < total && pair.sender.waitsnd() < 1024 {
            let size = (total - sent).min(chunk.len());
            pair.sender.send(&chunk[..size]).unwrap();
            sent += size;
        }

        recv_bytes += pair.step(start_ms + elapsed).await;

        // 曲线只在越过采样点时记录一次，避免 step 耗时不均导致补记
        while next_curve < CURVE_MS.len() && elapsed >= CURVE_MS[next_curve] {
            curve.push((CURVE_MS[next_curve], recv_bytes));
            next_curve += 1;
        }
        if elapsed >= WINDOW_MS && at_window == 0 {
            at_window = recv_bytes;
        }

        if recv_bytes >= total {
            total_ms = Some(elapsed);
            break;
        }
        if elapsed > DEADLINE_MS {
            break;
        }

        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
    }

    // 若在 300ms 之前就传完全部数据，窗口内的送达量就是总量本身，
    // 但此时指标被 total 截断，不代表该丢包率下的真实吞吐能力，需标注 saturated
    let saturated = total_ms.is_some_and(|v| v < WINDOW_MS);
    if at_window == 0 && recv_bytes > 0 {
        at_window = recv_bytes;
    }

    let (passed, dropped) = pair.stats();
    Result {
        at_window,
        curve,
        total_ms,
        passed,
        dropped,
        warmed,
        saturated,
    }
}

fn median(mut v: Vec<usize>) -> usize {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

#[tokio::main]
async fn main() {
    let mut total = DEFAULT_TOTAL;
    let mut seed = DEFAULT_SEED;
    let mut rounds = DEFAULT_ROUNDS;
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--total" if i + 1 < args.len() => {
                total = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--seed" if i + 1 < args.len() => {
                seed = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--rounds" if i + 1 < args.len() => {
                rounds = args[i + 1].parse().unwrap();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    let rounds = rounds.max(1);

    let cfg = KcpConfig::fast_mode();
    println!(
        "config: interval={}ms nodelay={} fastresend={} nocwnd={} mtu={} snd_wnd={} rcv_wnd={}",
        cfg.interval, cfg.nodelay, cfg.fastresend, cfg.nocwnd, cfg.mtu, cfg.snd_wnd, cfg.rcv_wnd
    );
    println!(
        "bench: total={}B window={}ms tick={}ms rounds={} seed={}",
        total, WINDOW_MS, TICK_MS, rounds, seed
    );
    println!();
    println!(
        "{:>5} | {:>12} | {:>12} | {:>11} | {:>12} | {:>9}",
        "loss", "cold@300ms", "warm@300ms", "warm KiB/s", "warm total", "warm drop%"
    );
    println!("{}", "-".repeat(78));

    for loss in LOSSES {
        let mut cold_vals = Vec::new();
        let mut warm_vals = Vec::new();
        let mut warm_totals: Vec<usize> = Vec::new();
        // 曲线按采样点收集各轮数值，最后逐点取中位数
        let mut curve_vals: Vec<Vec<usize>> = vec![Vec::new(); CURVE_MS.len()];
        let mut drop_ratio = 0.0f64;
        let mut warmed = false;
        let mut any_saturated = false;

        for r in 0..rounds {
            // 每轮换种子，避免同一随机序列反复命中同一种丢包模式
            let s = seed.wrapping_add(r as u64 * 0x9e37_79b9_7f4a_7c15);
            let cold = run(&cfg, total, loss, s, false).await;
            let warm = run(&cfg, total, loss, s, true).await;

            cold_vals.push(cold.at_window);
            warm_vals.push(warm.at_window);
            // 未跑完记为 DEADLINE_MS+1，与真实完成时间区分开
            warm_totals.push(
                warm.total_ms
                    .map(|v| v as usize)
                    .unwrap_or(DEADLINE_MS as usize + 1),
            );
            for (idx, (_, bytes)) in warm.curve.iter().enumerate() {
                if idx < curve_vals.len() {
                    curve_vals[idx].push(*bytes);
                }
            }
            if r == 0 {
                warmed = warm.warmed;
            }
            if warm.saturated {
                any_saturated = true;
            }
            let total_pkts = warm.passed + warm.dropped;
            if total_pkts > 0 {
                drop_ratio += warm.dropped as f64 / total_pkts as f64;
            }
        }

        let cold_med = median(cold_vals);
        let warm_med = median(warm_vals);
        let total_med = median(warm_totals);
        let kib = warm_med as f64 / (WINDOW_MS as f64 / 1000.0) / 1024.0;
        let total_str = if total_med > DEADLINE_MS as usize {
            format!(">{}ms", DEADLINE_MS)
        } else {
            format!("{}ms", total_med)
        };

        let mark = if any_saturated {
            " (saturated, raise --total)"
        } else {
            ""
        };
        println!(
            "{:>4.0}% | {:>12} | {:>12} | {:>11.1} | {:>12} | {:>8.1}%{}",
            loss * 100.0,
            cold_med,
            warm_med,
            kib,
            total_str,
            drop_ratio / rounds as f64 * 100.0,
            mark
        );
        let curve_str = CURVE_MS
            .iter()
            .enumerate()
            .filter(|(idx, _)| !curve_vals[*idx].is_empty())
            .map(|(idx, t)| format!("{t}ms={}", median(curve_vals[idx].clone())))
            .collect::<Vec<_>>()
            .join("  ");
        println!("      warm curve: {}  (warmed={})", curve_str, warmed);
    }
}
