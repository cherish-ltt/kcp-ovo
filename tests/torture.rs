//! 黑盒测试：极端网络形态与规模
//!
//! 与 `fuzz_transfer` / `outage` 的区别在于这里刻意叠加多种真实世界的恶劣条件：
//! 成簇丢包（隧道/弱信号连续丢一段）、延迟尖刺（队列堆积）、路径 MTU 黑洞
//! （中间设备丢弃超长报文）、双向同时断网、回绕点叠加重传。
//! 全部使用虚拟时钟与固定种子，失败可原样复现。

mod common;

use common::stress::{hostile_net, transfer_a_to_b, transfer_both};
use common::{Net, Rng, Sim, Solo, assert_same, checksum};
use kcp_ovo::KcpConfig;
use kcp_ovo::error::KcpErrorKind;

/// 成簇丢包（连续丢一段）下仍必须完整送达
#[tokio::test]
async fn burst_loss_transfer_is_byte_exact() {
    let mut rng = Rng::new(0x7011_0001);
    for round in 0..3u64 {
        let net = hostile_net(&mut rng);
        let cfg = KcpConfig { snd_wnd: 64, rcv_wnd: 128, ..KcpConfig::stream_mode() };
        let mut sim = Sim::new(cfg, net, 0x7011_0100 + round);
        let mss = sim.a.borrow().mss as usize;
        let payload = rng.bytes(mss * 200);

        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 3_000_000).await;
        assert!(ok, "[#{round}] 突发丢包链路未收敛，{ticks} tick");
        assert_same(&got, &payload);
        assert_eq!(sim.a.borrow().waitsnd(), 0);
    }
}

/// 延迟尖刺不得引发重传风暴
///
/// 个别包被额外拖延 250ms 时，超时判定若过于激进，会把仍在途的段当成丢包重发，
/// 使实际发包数远超段数。这类退化在「数据能送达」的断言下完全看不出来。
#[tokio::test]
async fn delay_spikes_do_not_cause_retransmit_storm() {
    let net = Net::default().with_delay(10).with_spike(0.03, 250);
    let cfg = KcpConfig { snd_wnd: 64, rcv_wnd: 128, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0x7011_0200);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(0x7011_0201).bytes(mss * 120);

    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 2_000_000).await;
    assert!(ok, "延迟尖刺下未收敛，{ticks} tick");
    assert_same(&got, &payload);

    let segments = payload.len().div_ceil(mss);
    let sent = sim.a.borrow().xmit as usize;
    assert!(sent <= segments * 3 + 32, "{segments} 段数据发出了 {sent} 个包，尖刺触发了重传风暴");
}

/// 路径 MTU 黑洞（中间设备丢弃超长报文）必须最终上报死链，而不是永久静默挂死
#[tokio::test]
async fn path_mtu_blackhole_surfaces_dead_link() {
    // 只有 28 字节的控制帧能穿过 200 字节的路径 MTU，数据帧全部被丢弃
    let mut sim = Sim::new(
        KcpConfig::stream_mode(),
        Net::default().with_delay(5).with_path_mtu(200),
        0x7011_0300,
    );
    sim.a.borrow_mut().dead_link = 6;
    sim.a.borrow_mut().send(&vec![0xA5u8; 8000]).unwrap();

    let mut err = None;
    for _ in 0..200_000 {
        match sim.tick().await {
            Ok(()) => {},
            Err(e) => {
                err = Some(e);
                break;
            },
        }
    }
    let err = err.expect("路径 MTU 黑洞应触发死链错误，而不是静默挂死");
    assert_eq!(err.kind(), KcpErrorKind::IoError, "错误类型应为死链：{err}");
    assert!(sim.a.borrow().nsnd_buf > 0, "死链时在途段不应被误清");
}

/// 双向同时断网后，两个方向都必须补齐且不重复
#[tokio::test]
async fn simultaneous_blackout_recovers_both_directions() {
    let net = Net::loss(0.05).with_delay(8).with_blackout(200, 400);
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 64, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0x7011_0400);
    let mss = sim.a.borrow().mss as usize;
    let mut rng = Rng::new(0x7011_0401);
    let a_payload = rng.bytes(mss * 120);
    let b_payload = rng.bytes(mss * 120);

    let (ok, got_b, got_a, ticks) =
        transfer_both(&mut sim, &a_payload, &b_payload, 4_000_000).await;
    assert!(ok, "双向断网后未恢复，{ticks} tick");
    assert_same(&got_b, &a_payload);
    assert_same(&got_a, &b_payload);
    assert_eq!(checksum(&got_b), checksum(&a_payload));
    assert_eq!(checksum(&got_a), checksum(&b_payload));
    assert_eq!(sim.a.borrow().waitsnd(), 0);
    assert_eq!(sim.b.borrow().waitsnd(), 0);
}

/// SN 回绕点叠加丢包与断网：旧段与新段不得混淆，数据不得重复或丢失
#[tokio::test]
async fn wraparound_with_loss_and_blackout() {
    let start = u32::MAX - 12;
    let net = Net::loss(0.15).with_delay(6).with_jitter(3).with_blackout(120, 250);
    let cfg = KcpConfig { snd_wnd: 16, rcv_wnd: 32, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0x7011_0500);
    sim.a.borrow_mut().snd_nxt = start;
    sim.a.borrow_mut().snd_una = start;
    sim.b.borrow_mut().rcv_nxt = start;

    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(0x7011_0501).bytes(mss * 64);
    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 3_000_000).await;
    assert!(ok, "回绕 + 丢包 + 断网未收敛，{ticks} tick");
    assert_same(&got, &payload);
    assert_eq!(sim.b.borrow().nrcv_buf, 0, "交付完成后不应残留乱序缓存");
}

/// 100 条连接同时承受成簇丢包，各自数据必须完整且不串台
#[tokio::test]
async fn hundred_connections_with_burst_loss_stay_isolated() {
    const CLIENTS: usize = 100;
    let mut rng = Rng::new(0x7011_0600);
    let cfg = KcpConfig { snd_wnd: 16, rcv_wnd: 32, ..KcpConfig::stream_mode() };

    let mut sims = Vec::with_capacity(CLIENTS);
    let mut sent: Vec<Vec<u8>> = Vec::with_capacity(CLIENTS);
    for i in 0..CLIENTS {
        let sim = Sim::new(cfg.clone(), hostile_net(&mut rng), 0x7011_0600 + i as u64);
        let mss = sim.a.borrow().mss as usize;
        let mut payload = rng.bytes(mss * 4);
        payload[0] = i as u8;
        sim.a.borrow_mut().send(&payload).unwrap();
        sent.push(payload);
        sims.push(sim);
    }

    let mut got: Vec<Vec<u8>> = vec![Vec::new(); CLIENTS];
    // 突发丢包叠加 nodelay 的 RTO 翻倍，恢复要几十秒虚拟时间，上限按此放宽
    for _ in 0..300_000 {
        let mut all_done = true;
        for (i, sim) in sims.iter_mut().enumerate() {
            sim.tick().await.unwrap();
            got[i].extend_from_slice(&sim.take_b());
            if got[i].len() < sent[i].len() || sim.a.borrow().waitsnd() != 0 {
                all_done = false;
            }
        }
        if all_done {
            break;
        }
    }

    for (i, (g, s)) in got.iter().zip(sent.iter()).enumerate() {
        assert_eq!(g.len(), s.len(), "连接 {i} 长度不一致");
        assert_eq!(checksum(g), checksum(s), "连接 {i} 内容不一致");
    }
}

/// 接收端暂停读取导致零窗口时，探测流量必须有界；恢复读取后必须续传到底
#[tokio::test]
async fn stalled_receiver_recovers_with_bounded_probe_traffic() {
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 8, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, Net::loss(0.1).with_delay(10), 0x7011_0700);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(0x7011_0701).bytes(mss * 60);
    sim.a.borrow_mut().send(&payload).unwrap();

    // 前 3 秒完全不读：接收队列填满后对端窗口归零
    let before = sim.a.borrow().xmit;
    sim.run(3_000).await.unwrap();
    let stalled_packets = sim.a.borrow().xmit - before;
    assert!(sim.b.borrow().nrcv_que > 0, "接收端应已积压数据");
    assert!(sim.a.borrow().rmt_wnd == 0, "对端窗口应已归零");
    assert!(stalled_packets < 200, "零窗口 3 秒内发出 {stalled_packets} 个包，探测或重传失控");

    // 恢复读取后必须续传完成
    let mut got = sim.take_b();
    for _ in 0..2_000_000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 对端窗口长期为 0 时，本端发送队列不得被反复塞入新段
#[tokio::test]
async fn zero_window_does_not_inflate_send_buffer() {
    let mut solo = Solo::new(KcpConfig { snd_wnd: 32, ..KcpConfig::stream_mode() });
    let conv = solo.kcp.borrow().conv;
    solo.kcp
        .borrow_mut()
        .input(&common::stress::control_packet(conv, kcp_ovo::KcpCmd::Wins, 0))
        .unwrap();
    solo.kcp.borrow_mut().send(&vec![0x33u8; 8000]).unwrap();

    for _ in 0..2_000 {
        solo.step(20).await.unwrap();
    }
    assert_eq!(solo.kcp.borrow().nsnd_buf, 0, "对端零窗口时不得把段移入 snd_buf");
    assert!(solo.kcp.borrow().nsnd_que > 0, "数据应留在发送队列里等待窗口");
}
