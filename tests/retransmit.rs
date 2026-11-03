//! 黑盒测试：快速重传与死链检测
//!
//! 这两条路径在常规收发中不常触发，却是可靠性最关键的兜底：
//! - 快速重传：靠重复 ACK 提前补发丢失段，而不是等 RTO
//! - 死链检测：重传次数超过 dead_link 后必须显式报错，而不是无限重试

mod common;

use common::stress::{Sink, control_packet, transfer_a_to_b};
use common::{Net, Rng, Sim, Solo, assert_same};
use kcp_ovo::KcpConfig;
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::{IKCP_RTO_MAX, KcpCmd};

async fn assert_timeout_schedule(nodelay: bool, schedule: &[(u64, u64)]) {
    for start in [0, u64::from(u32::MAX) - 100, u64::MAX - 100] {
        let mut sink = Sink::new(KcpConfig { nodelay, interval: 1, ..KcpConfig::default() });
        sink.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 128)).unwrap();
        sink.kcp.borrow_mut().send(b"lost").unwrap();
        let mut now = start;
        for (index, &(rto, deadline)) in schedule.iter().enumerate() {
            let deadline = start.wrapping_add(deadline);
            sink.update(now).await.unwrap();
            assert_eq!(sink.count_cmd(KcpCmd::Push), 1);
            sink.clear();
            {
                let kcp = sink.kcp.borrow();
                let seg = kcp.snd_buf.front().unwrap();
                assert_eq!((seg.rto, seg.resendts), (rto, deadline), "retry={index}");
                assert_eq!(seg.xmit, index as u32 + 1);
            }
            sink.update(deadline.wrapping_sub(1)).await.unwrap();
            assert_eq!(sink.count_cmd(KcpCmd::Push), 0, "early retry={index}");
            assert_eq!(sink.kcp.borrow().check(deadline.wrapping_sub(1)), deadline);
            now = deadline;
        }
    }
}

#[tokio::test]
async fn normal_timeout_backoff_uses_new_deadline() {
    assert_timeout_schedule(
        false,
        &[(200, 225), (400, 625), (800, 1425), (1600, 3025), (3200, 6225), (6400, 12625)],
    )
    .await;
}

#[tokio::test]
async fn fast_timeout_backoff_uses_new_deadline() {
    assert_timeout_schedule(
        true,
        &[(200, 200), (300, 500), (450, 950), (675, 1625), (1012, 2637), (1518, 4155)],
    )
    .await;
}

#[tokio::test]
async fn timeout_backoff_is_bounded_and_obeys_dead_link() {
    for nodelay in [false, true] {
        for (old_rto, measured_rto) in [(200, 1000), (IKCP_RTO_MAX - 1, 200), (u64::MAX, 200)] {
            let mut sink = Sink::new(KcpConfig { nodelay, interval: 1, ..KcpConfig::default() });
            sink.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 128)).unwrap();
            sink.kcp.borrow_mut().send(b"lost").unwrap();
            sink.update(0).await.unwrap();
            let mut now = {
                let mut kcp = sink.kcp.borrow_mut();
                kcp.dead_link = 3;
                kcp.rx_rto = measured_rto;
                let seg = kcp.snd_buf.front_mut().unwrap();
                seg.rto = old_rto;
                seg.resendts
            };
            let mut expected = old_rto;
            for xmit in 2..=3 {
                expected = if expected >= IKCP_RTO_MAX - 1 {
                    IKCP_RTO_MAX
                } else if nodelay {
                    expected + expected / 2
                } else {
                    expected + expected.max(measured_rto)
                };
                sink.clear();
                sink.update(now).await.unwrap();
                assert_eq!(sink.count_cmd(KcpCmd::Push), 1);
                let kcp = sink.kcp.borrow();
                let seg = kcp.snd_buf.front().unwrap();
                assert_eq!((seg.rto, seg.resendts, seg.xmit), (expected, now + expected, xmit));
                now = seg.resendts;
            }
            sink.clear();
            let error = sink.update(now).await.unwrap_err();
            assert!(error.to_string().contains("retransmissions"));
            assert_eq!(sink.count_cmd(KcpCmd::Push), 0);
            assert_eq!(sink.kcp.borrow().snd_buf.front().unwrap().xmit, 3);
        }
    }
}

#[tokio::test]
async fn timeout_and_fastack_are_exclusive_and_ack_recovery_preserves_rtt() {
    for nodelay in [false, true] {
        for timeout in [false, true] {
            let cfg = KcpConfig { nodelay, interval: 1, fastresend: 2, ..KcpConfig::default() };
            let mut sender = Sink::new(cfg.clone());
            let mut receiver = Sink::new(cfg);
            sender.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 128)).unwrap();
            sender.kcp.borrow_mut().send(b"lost").unwrap();
            sender.update(0).await.unwrap();
            let at = {
                let mut kcp = sender.kcp.borrow_mut();
                let seg = kcp.snd_buf.front_mut().unwrap();
                seg.fastack = 2;
                if timeout { seg.resendts } else { 20 }
            };
            sender.clear();
            sender.update(at).await.unwrap();
            assert_eq!(sender.count_cmd(KcpCmd::Push), 1);
            {
                let kcp = sender.kcp.borrow();
                let seg = kcp.snd_buf.front().unwrap();
                let rto = if !timeout {
                    200
                } else if nodelay {
                    300
                } else {
                    400
                };
                assert_eq!((seg.xmit, seg.fastack, seg.rto, seg.resendts), (2, 0, rto, at + rto));
            }
            receiver.kcp.borrow_mut().input(&sender.emitted()[0]).unwrap();
            assert_eq!(&receiver.kcp.borrow_mut().recv().unwrap()[..], b"lost");
            receiver.update(at + 10).await.unwrap();
            sender.update(at + 10).await.unwrap();
            for ack in receiver.emitted() {
                sender.kcp.borrow_mut().input(&ack).unwrap();
            }
            {
                let kcp = sender.kcp.borrow();
                assert_eq!(kcp.waitsnd(), 0);
                assert_eq!((kcp.rx_srtt, kcp.rx_rttval, kcp.rx_rto), (0, 0, 200));
            }
            sender.clear();
            receiver.clear();
            sender.kcp.borrow_mut().send(b"fresh").unwrap();
            sender.update(at + 11).await.unwrap();
            assert_eq!(sender.kcp.borrow().snd_buf.front().unwrap().rto, 200);
            receiver.kcp.borrow_mut().input(&sender.emitted()[0]).unwrap();
            assert_eq!(&receiver.kcp.borrow_mut().recv().unwrap()[..], b"fresh");
            receiver.update(at + 21).await.unwrap();
            sender.update(at + 21).await.unwrap();
            for ack in receiver.emitted() {
                sender.kcp.borrow_mut().input(&ack).unwrap();
            }
            assert_eq!(sender.kcp.borrow().waitsnd(), 0);
            assert_eq!(sender.kcp.borrow().rx_srtt, 10);
        }
    }
}

#[tokio::test]
async fn backoff_modes_deliver_on_clean_burst_and_blackout_links() {
    let payload = Rng::new(0x38).bytes(1024 * 1024);
    for nodelay in [false, true] {
        for (name, net) in [
            ("clean", Net::default()),
            ("burst", Net::default().with_burst(0.03, 0.5)),
            ("blackout", Net::default().with_blackout(0, 3000)),
        ] {
            let cfg = KcpConfig { nodelay, interval: 10, ..KcpConfig::stream_mode() };
            let mut sim = Sim::new(cfg, net, 0x38);
            let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 120_000).await;
            assert!(ok, "nodelay={nodelay} link={name} stalled after {ticks} ticks");
            assert_same(&got, &payload);
            assert_eq!(sim.a.borrow().waitsnd(), 0);
            println!(
                "nodelay={nodelay} link={name} bytes={} ticks={ticks} output_packets={}",
                got.len(),
                sim.a.borrow().xmit + sim.b.borrow().xmit
            );
        }
    }
}

/// 单个段持续丢失时，应通过快速重传补齐，而不是等到 RTO
///
/// 用可控丢包模拟：链路只丢第一遍的第 0 段，之后正常。
#[tokio::test]
async fn fast_retransmit_recovers_lost_segment() {
    // 高丢包让首轮必然有段丢失，后续重传能成功
    let net = Net::loss(0.35).with_delay(5);
    let mut sim = Sim::new(KcpConfig { fastresend: 2, ..KcpConfig::stream_mode() }, net, 71);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(71).bytes(mss * 12);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(30000).await.unwrap();
    assert_same(&got, &payload);
}

/// fastresend=0 时快速重传应被禁用，但仍靠 RTO 完成传输
#[tokio::test]
async fn fastresend_disabled_still_completes() {
    let cfg = KcpConfig { fastresend: 0, ..KcpConfig::stream_mode() };
    let net = Net::loss(0.2).with_delay(5);
    let mut sim = Sim::new(cfg, net, 72);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(72).bytes(mss * 6);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(60000).await.unwrap();
    assert_same(&got, &payload);
}

/// 重传次数超过 dead_link 后，update 必须返回错误而不是无限重试
#[tokio::test]
async fn dead_link_detected_after_too_many_retransmits() {
    let mut solo = Solo::new(KcpConfig { fastresend: 2, ..KcpConfig::fast_mode() });
    // 对端窗口已知且足够大，但没有对端回 ACK → 会一直重传
    solo.kcp.borrow_mut().rmt_wnd = 1024;
    solo.kcp.borrow_mut().peer_seen = true;
    solo.kcp.borrow_mut().dead_link = 3;
    solo.kcp.borrow_mut().send(b"never-acked").unwrap();

    let mut saw_error = false;
    for _ in 0..20000 {
        match solo.step(50).await {
            Ok(_) => {},
            Err(e) => {
                assert_eq!(e.kind(), kcp_ovo::error::KcpErrorKind::IoError);
                assert!(e.to_string().contains("retransmission"), "错误信息应指出重传超限: {e}");
                saw_error = true;
                break;
            },
        }
    }
    assert!(saw_error, "重传超过 dead_link 后必须报死链错误");
}

/// dead_link 未满时不应误报死链
#[tokio::test]
async fn no_dead_link_before_threshold() {
    let mut solo = Solo::new(KcpConfig::fast_mode());
    solo.kcp.borrow_mut().rmt_wnd = 1024;
    solo.kcp.borrow_mut().peer_seen = true;
    solo.kcp.borrow_mut().dead_link = 100;
    solo.kcp.borrow_mut().send(b"data").unwrap();

    for _ in 0..200 {
        if solo.step(20).await.is_err() {
            panic!("未达 dead_link 阈值不应报死链");
        }
    }
    assert!(solo.kcp.borrow().nsnd_buf > 0, "数据应仍挂在发送缓冲等待重传");
}

/// 大窗口 + 高丢包下快速重传也应正确工作
#[tokio::test]
async fn fast_retransmit_with_large_window() {
    let cfg = KcpConfig { stream_mode: true, ..KcpConfig::fast_mode_with_wnd(256, 256) };
    let net = Net::loss(0.25).with_delay(8).with_reorder(0.3, 0.0);
    let mut sim = Sim::new(cfg, net, 73);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(73).bytes(mss * 50);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(60000).await.unwrap();
    assert_same(&got, &payload);
}

/// 死链阈值默认值应与 C 原版一致（IKCP_DEADLINK = 20 次）
#[test]
fn default_dead_link_matches_ikcp() {
    let kcp = Kcp::new(1, KcpConfig::default()).unwrap();
    assert_eq!(kcp.dead_link, kcp_ovo::helper::IKCP_DEADLINK);
    assert_eq!(kcp.dead_link, 20);
}
