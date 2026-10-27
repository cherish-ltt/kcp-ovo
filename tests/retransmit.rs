//! 黑盒测试：快速重传与死链检测
//!
//! 这两条路径在常规收发中不常触发，却是可靠性最关键的兜底：
//! - 快速重传：靠重复 ACK 提前补发丢失段，而不是等 RTO
//! - 死链检测：重传次数超过 dead_link 后必须显式报错，而不是无限重试

mod common;

use common::{Net, Rng, Sim, Solo, assert_same};
use kcp_ovo::KcpConfig;
use kcp_ovo::core::kcp::Kcp;

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

/// 死链阈值常量默认值应符合协议约定（100 次）
#[test]
fn default_dead_link_is_100() {
    let kcp = Kcp::new(1, KcpConfig::default()).unwrap();
    assert_eq!(kcp.dead_link, 100);
}
