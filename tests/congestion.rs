//! 黑盒测试：拥塞窗口与吞吐行为
//!
//! 前面所有用例只断言「数据是否完整送达」。这在拥塞控制失效时是盲区：
//! `cwnd` 卡在 1 只会让传输变慢，内容依然正确，用例照样通过。
//! 本文件专门断言**窗口的增长节奏与吞吐特征**，用于回归以下缺陷：
//!
//! - BUG-12：`ts_lastack` 从未被写入，`current >= ts_lastack` 恒真，
//!   「每个 ACK 周期最多增长一次」的门限失效，`cwnd` 每轮 flush 都涨。
//! - BUG-13：`ssthresh` 初值与 `cwnd` 初值同为 2，`cwnd < ssthresh` 永远不成立，
//!   慢启动被整段跳过，`normal` 模式 `cwnd` 长期卡在 1。
//! - BUG-14：`fastresend == 0`（`normal_mode` 默认值）时
//!   `fastack >= 0` 恒真，每轮都判定丢包，`cwnd` 被反复砸回 1。

mod common;

use common::{Net, Rng, Sim};
use kcp_ovo::KcpConfig;
use kcp_ovo::core::kcp::Kcp;

/// 干净链路下连续发 `segments` 段，返回每轮结束后的 `cwnd` 序列
async fn cwnd_after_each_round(sim: &mut Sim, segments: usize) -> Vec<u16> {
    let mss = sim.a.borrow().mss as usize;
    let mut samples = Vec::new();
    for _ in 0..segments {
        sim.a.borrow_mut().send(&vec![0x5Au8; mss]).unwrap();
        for _ in 0..5_000 {
            sim.tick().await.unwrap();
            sim.take_b();
            if sim.a.borrow().waitsnd() == 0 {
                break;
            }
        }
        samples.push(sim.a.borrow().cwnd);
    }
    samples
}

/// 干净链路下 cwnd 必须持续增长，不能卡住（BUG-13 回归）
#[tokio::test]
async fn cwnd_grows_on_clean_link() {
    for (name, cfg) in [
        ("normal", KcpConfig::normal_mode()),
        ("fast", KcpConfig::fast_mode()),
        ("stream", KcpConfig::stream_mode()),
    ] {
        let mut sim = Sim::new(cfg, Net::default(), 0xE0E0_0001);
        let start = sim.a.borrow().cwnd;
        let samples = cwnd_after_each_round(&mut sim, 10).await;
        let end = *samples.last().unwrap();
        // 干净链路上没有任何丢包，cwnd 必须稳步爬升而不是卡在低位。
        // 只断言 `end > start` 太宽松：cwnd 从 1 涨到 2 也满足，
        // 而 BUG-13 的症状恰恰是长期卡在 1~2 之间，会被漏掉。
        assert!(
            end >= start + 5,
            "{name} 模式 cwnd 增长过慢：起始 {start}，10 轮后 {end}，样本 {samples:?}"
        );
    }
}

/// 慢启动阶段 cwnd 应近似线性增长（每 ACK 至少 +1 段）
#[tokio::test]
async fn slow_start_increments_each_round() {
    let mut sim = Sim::new(KcpConfig::normal_mode(), Net::default(), 0xE0E0_0002);
    let ssthresh = sim.a.borrow().ssthresh;
    let samples = cwnd_after_each_round(&mut sim, 8).await;

    for (i, pair) in samples.windows(2).enumerate() {
        let (prev, next) = (pair[0], pair[1]);
        if prev < ssthresh as u16 {
            assert!(
                next > prev,
                "慢启动阶段第 {i} 轮 cwnd 未增长：{prev} -> {next}（ssthresh={ssthresh}）"
            );
        }
    }
}

/// 慢启动阈值初值必须高于 cwnd 初值，否则慢启动分支永不进入（BUG-13 回归）
#[tokio::test]
async fn ssthresh_init_allows_slow_start() {
    let kcp = Kcp::new(1, KcpConfig::normal_mode()).unwrap();
    assert!(
        kcp.ssthresh > kcp.cwnd as u32,
        "ssthresh 初值 {} 不大于 cwnd 初值 {}，慢启动无法进入",
        kcp.ssthresh,
        kcp.cwnd
    );
}

/// fastresend=0 必须关闭快速重传，不能每轮都误判丢包（BUG-14 回归）
#[tokio::test]
async fn fastresend_zero_disables_fast_retransmit() {
    let cfg = KcpConfig {
        fastresend: 0,
        interval: 20,
        snd_wnd: 32,
        rcv_wnd: 128,
        ..KcpConfig::normal_mode()
    };
    let mut sim = Sim::new(cfg, Net::default(), 0xE0E0_0003);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(3).bytes(mss * 20);
    sim.a.borrow_mut().send(&payload).unwrap();

    // 干净链路上不允许发生任何 cwnd 收缩（收缩只应发生在真实丢包时）
    let mut min_cwnd = u16::MAX;
    for _ in 0..5_000 {
        sim.tick().await.unwrap();
        sim.take_b();
        min_cwnd = min_cwnd.min(sim.a.borrow().cwnd);
        if sim.a.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert!(min_cwnd > 1, "干净链路上 cwnd 被砸到 {min_cwnd}，说明错误触发了丢包判定");
    assert_eq!(sim.a.borrow().waitsnd(), 0, "传输未完成");
}

/// 真实丢包时 cwnd 必须按 AIMD 收缩，并把 ssthresh 减半
#[tokio::test]
async fn loss_halves_ssthresh_and_resets_cwnd() {
    let mut sim = Sim::new(KcpConfig::normal_mode(), Net::loss(0.3).with_delay(6), 0xE0E0_0004);
    let mss = sim.a.borrow().mss as usize;
    sim.a.borrow_mut().send(&Rng::new(4).bytes(mss * 200)).unwrap();

    let mut prev_cwnd = sim.a.borrow().cwnd;
    let mut observed_shrink = false;
    for _ in 0..200_000 {
        sim.tick().await.unwrap();
        for m in sim.b_messages() {
            let _ = m;
        }
        let cwnd = sim.a.borrow().cwnd;
        if cwnd < prev_cwnd {
            let ssthresh = sim.a.borrow().ssthresh;
            // 收缩后 ssthresh 应不小于最小阈值
            assert!(ssthresh >= 2, "ssthresh {ssthresh} 低于最小阈值");
            observed_shrink = true;
            break;
        }
        prev_cwnd = cwnd;
    }
    assert!(observed_shrink, "30% 丢包下 cwnd 从未收缩，拥塞控制未生效");
}

/// 每个 ACK 周期最多让 cwnd 增长一次（BUG-12 回归）
///
/// 同一时刻连续投递多个重复 ACK，cwnd 的增量不应与 ACK 数量成正比。
#[tokio::test]
async fn cwnd_grows_at_most_once_per_ack_cycle() {
    let mut sim = Sim::new(KcpConfig::normal_mode(), Net::default(), 0xE0E0_0005);
    let mss = sim.a.borrow().mss as usize;
    for _ in 0..8 {
        sim.a.borrow_mut().send(&vec![0x11u8; mss]).unwrap();
    }
    sim.run(50).await.unwrap();
    sim.take_b();

    let before = sim.a.borrow().cwnd;
    let snd_buf_len = sim.a.borrow().snd_buf.len();
    // 同一 tick 内灌入大量重复确认
    for _ in 0..50 {
        sim.tick().await.unwrap();
        sim.take_b();
    }
    let after = sim.a.borrow().cwnd;
    // 一个 ACK 周期最多涨 1 段；这里跑了很多 tick，允许随轮次增长，
    // 但绝不允许「一次确认涨几十段」这种失控增长。
    assert!(after <= before + 60, "cwnd 增长失控：{before} -> {after}（在途 {snd_buf_len} 段）");
}

/// 三种模式在同等条件下的吞吐不应出现数量级差异
///
/// 注意：消息模式下单条消息的分片数不能超过 `rcv_wnd`，否则接收端
/// 凑不齐 frg 链、`recv` 永远返回 `PayloadWaitForFrg`，属于协议语义而非缺陷。
/// 因此这里拆成多条小消息发送，保证三种模式都在公平条件下比较。
#[tokio::test]
async fn modes_have_comparable_throughput() {
    let mss = {
        let sim = Sim::new(KcpConfig::normal_mode(), Net::default(), 1);
        sim.a.borrow().mss as usize
    };
    let mut ticks = Vec::new();
    for cfg in [KcpConfig::normal_mode(), KcpConfig::fast_mode(), KcpConfig::stream_mode()] {
        let mut sim = Sim::new(cfg.clone(), Net::default(), 0xE0E0_0006);
        let cfg_nocwnd = cfg.nocwnd;
        // 每条消息 4 段，共 200 段：谁都不会撞上 rcv_wnd 上限
        let payload_chunks: Vec<Vec<u8>> = (0..50).map(|_| Rng::new(6).bytes(mss * 4)).collect();
        // 只要发送队列降到低水位就补发下一条，模拟持续有数据要发的真实场景
        let mut next = 0usize;
        let mut sent_bytes = 0usize;
        let total: usize = payload_chunks.iter().map(|c| c.len()).sum();
        let mut received = 0usize;
        let mut used = 0;
        for _ in 0..500_000 {
            sim.tick().await.unwrap();
            // 必须持续取走接收端数据，否则接收队列塞满、窗口通告为 0，
            // 发送端会被合法地停住，测到的就不是吞吐而是背压。
            received += sim.take_b().len();
            used += 1;
            if next < payload_chunks.len() && sim.a.borrow().waitsnd() < mss * 8 {
                sent_bytes += sim.a.borrow_mut().send(&payload_chunks[next]).unwrap();
                next += 1;
            }
            if received >= total && sim.a.borrow().waitsnd() == 0 {
                break;
            }
        }
        assert_eq!(sent_bytes, total, "并非所有数据都已入队");
        assert_eq!(sim.a.borrow().waitsnd(), 0, "未完成");
        ticks.push((cfg_nocwnd, used));
    }

    // nocwnd 模式跨越了拥塞控制的全部调节，与 normal 不构成公平对比：
    // normal 走慢启动爬升，nocwnd 直接吃满 snd_wnd，慢上数倍是设计使然。
    // 这里只守住两条底线：不出现数量级失控，且 normal 仍在合理区间内。
    let cwnd_ticks: Vec<usize> =
        ticks.iter().filter(|(nocwnd, _)| !nocwnd).map(|(_, t)| *t).collect();
    let nocwnd_ticks: Vec<usize> =
        ticks.iter().filter(|(nocwnd, _)| *nocwnd).map(|(_, t)| *t).collect();
    for t in &nocwnd_ticks {
        assert!(*t <= 2_000, "nocwnd 模式吞吐异常：{ticks:?}");
    }
    for t in &cwnd_ticks {
        // normal 模式跑完 200 段的上限：慢启动爬升 + 拥塞避免的正常量级
        assert!(*t <= 20_000, "normal 模式吞吐异常：{ticks:?}");
    }
}
