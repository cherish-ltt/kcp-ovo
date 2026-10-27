//! 黑盒测试：劣化链路下的可靠性
//!
//! 模拟真实网络的丢包、乱序、重复投递，验证重传/快速重传/去重是否正确。
//! 这是最容易暴露协议缺陷的一类场景。

mod common;

use common::{Net, Rng, Sim, assert_same};
use kcp_ovo::KcpConfig;

/// 20% 丢包下数据必须完整送达
#[tokio::test]
async fn lossy_20_percent_delivers_all() {
    let net = Net::loss(0.2);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 21);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(21).bytes(mss * 8);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(20000).await.unwrap();
    assert_same(&got, &payload);
}

/// 40% 丢包下数据必须完整送达（验证重传不丢也不重）
#[tokio::test]
async fn lossy_40_percent_delivers_all() {
    let net = Net::loss(0.4);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 22);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(22).bytes(mss * 4);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(40000).await.unwrap();
    assert_same(&got, &payload);
}

/// 60% 丢包下仍应最终送达（极劣链路，给足时间）
#[tokio::test]
async fn lossy_60_percent_delivers_all() {
    let net = Net::loss(0.6);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 23);
    let payload = Rng::new(23).bytes(200);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(120000).await.unwrap();
    assert_same(&got, &payload);
}

/// 重复投递不应让接收端产生重复数据
#[tokio::test]
async fn duplicate_packets_are_deduped() {
    let net = Net { duplicate: 1.0, ..Net::default() };
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 24);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(24).bytes(mss * 6);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(4000).await.unwrap();
    assert_same(&got, &payload);
}

/// 乱序到达（含抖动）不应影响交付顺序与内容
#[tokio::test]
async fn reordered_delivery_is_reassembled() {
    let net = Net::default().with_jitter(25).with_reorder(0.9, 0.0);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 25);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(25).bytes(mss * 10 + 33);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(20000).await.unwrap();
    assert_same(&got, &payload);
}

/// 丢包 + 乱序 + 重复 的组合劣化链路
#[tokio::test]
async fn combined_impairment_delivers_all() {
    let net = Net::loss(0.3).with_jitter(20).with_reorder(0.5, 0.3);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 26);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(26).bytes(mss * 6);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(40000).await.unwrap();
    assert_same(&got, &payload);
}

/// 高延迟链路上仍然完整（验证 RTO 随延迟自适应）
#[tokio::test]
async fn high_latency_path() {
    let net = Net::default().with_delay(250);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 27);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(27).bytes(mss * 3);
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(20000).await.unwrap();
    assert_same(&got, &payload);
}

/// 双向同时传输在丢包下依然各自完整
#[tokio::test]
async fn bidirectional_under_loss() {
    let net = Net::loss(0.25);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 28);
    let mss = sim.a.borrow().mss as usize;
    let a_payload = Rng::new(28).bytes(mss * 4);
    let b_payload = Rng::new(29).bytes(mss * 4 + 55);
    sim.a.borrow_mut().send(&a_payload).unwrap();
    sim.b.borrow_mut().send(&b_payload).unwrap();

    let mut got_a = Vec::new();
    let mut got_b = Vec::new();
    for _ in 0..30000 {
        sim.tick().await.unwrap();
        got_a.extend_from_slice(&sim.take_a());
        got_b.extend_from_slice(&sim.take_b());
        if got_a.len() >= b_payload.len() && got_b.len() >= a_payload.len() {
            break;
        }
    }
    assert_same(&got_b, &a_payload);
    assert_same(&got_a, &b_payload);
}

/// 非流模式下丢包也不应把一条消息拆成两条交付
#[tokio::test]
async fn message_boundary_survives_loss() {
    let net = Net::loss(0.3);
    let mut sim = Sim::new(KcpConfig::fast_mode(), net, 30);
    let mss = sim.a.borrow().mss as usize;
    let msg = Rng::new(30).bytes(mss * 2 + 40);
    sim.a.borrow_mut().send(&msg).unwrap();

    sim.run(40000).await.unwrap();
    let got = sim.b_messages();
    assert_eq!(got.len(), 1, "一条消息必须整体交付一次，实际收到 {} 条", got.len());
    assert_same(&got[0], &msg);
}

/// 链路完全断开后恢复，应继续完成未确认的传输
#[tokio::test]
async fn recovers_after_outage() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 31);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(31).bytes(mss * 5);
    sim.a.borrow_mut().send(&payload).unwrap();

    // 先正常传一小段
    sim.run(50).await.unwrap();
    let mut got = sim.take_b();

    // 完全断网一段时间
    let lost = sim.net.loss;
    sim.net.loss = 1.0;
    sim.run(3000).await.unwrap();
    got.extend_from_slice(&sim.take_b());

    // 恢复链路
    sim.net.loss = lost;
    for _ in 0..40000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert_same(&got, &payload);
}
