//! 黑盒测试：基础传输正确性
//!
//! 覆盖真实使用场景下的数据完整性、大小边界、双向并发与多轮对话。

mod common;

use common::{Net, Rng, Sim, assert_same};
use kcp_ovo::KcpConfig;

/// 小消息在干净链路上应完整送达
#[tokio::test]
async fn small_message_roundtrip() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 1);
    let payload = b"kcp-ovo hello world".to_vec();
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(200).await.unwrap();
    assert_same(&got, &payload);
}

/// 恰好等于 MSS 的单段边界
#[tokio::test]
async fn exactly_one_mss() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 2);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(2).bytes(mss);
    sim.a.borrow_mut().send(&payload).unwrap();
    assert_eq!(sim.a.borrow().waitsnd(), 1, "MSS 长度应只切出 1 段");

    let got = sim.drain_a_to_b(200).await.unwrap();
    assert_same(&got, &payload);
}

/// MSS + 1 字节应切成 2 段，且尾段只有 1 字节
#[tokio::test]
async fn mss_plus_one() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 3);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(3).bytes(mss + 1);
    sim.a.borrow_mut().send(&payload).unwrap();
    assert_eq!(sim.a.borrow().waitsnd(), 2);

    let got = sim.drain_a_to_b(400).await.unwrap();
    assert_same(&got, &payload);
}

/// 空数据不应产生任何段，也不应报错
#[tokio::test]
async fn empty_send_is_noop() {
    let sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 4);
    let n = sim.a.borrow_mut().send(&[]).unwrap();
    assert_eq!(n, 0);
    assert_eq!(sim.a.borrow().waitsnd(), 0);
}

/// 多字节内容的完整性：逐字节可区分，确保没有错位/覆盖
#[tokio::test]
async fn payload_bytes_are_bit_exact() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 5);
    let mss = sim.a.borrow().mss as usize;
    let mut payload = Vec::new();
    for i in 0..(mss * 3 + 17) {
        payload.push((i % 251) as u8);
    }
    sim.a.borrow_mut().send(&payload).unwrap();

    let got = sim.drain_a_to_b(800).await.unwrap();
    assert_same(&got, &payload);
}

/// 非流模式：单条消息应作为一条完整消息交付，不被拆散
#[tokio::test]
async fn message_mode_preserves_boundary() {
    let mut sim = Sim::new(KcpConfig::fast_mode(), Net::default(), 6);
    let mss = sim.a.borrow().mss as usize;
    let msg = Rng::new(6).bytes(mss * 2 + 100);
    sim.a.borrow_mut().send(&msg).unwrap();

    sim.run(600).await.unwrap();
    let got = sim.b_messages();
    assert_eq!(got.len(), 1, "非流模式下一条消息应只被 recv 一次");
    assert_same(&got[0], &msg);
}

/// 非流模式：连续两条消息必须各自独立交付，不能粘成一体
#[tokio::test]
async fn message_mode_two_messages_separate() {
    let mut sim = Sim::new(KcpConfig::fast_mode(), Net::default(), 7);
    let first = b"first-message".to_vec();
    let second = b"second-message".to_vec();
    sim.a.borrow_mut().send(&first).unwrap();
    sim.a.borrow_mut().send(&second).unwrap();

    sim.run(600).await.unwrap();
    let got = sim.b_messages();
    assert_eq!(got.len(), 2, "两条消息应分别交付");
    assert_same(&got[0], &first);
    assert_same(&got[1], &second);
}

/// 流模式：多次 send 后，对端可以一次性或分批读回同一串字节
#[tokio::test]
async fn stream_mode_multi_send_is_contiguous() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 8);
    let mut expect = Vec::new();
    for i in 0..5u8 {
        let part = vec![i; 300];
        expect.extend_from_slice(&part);
        sim.a.borrow_mut().send(&part).unwrap();
    }

    let got = sim.drain_a_to_b(800).await.unwrap();
    assert_same(&got, &expect);
}

/// 双向同时传输：两端各自收全对端数据，互不干扰
#[tokio::test]
async fn bidirectional_concurrent_transfer() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 9);
    let mss = sim.a.borrow().mss as usize;
    let a_payload = Rng::new(9).bytes(mss * 6);
    let b_payload = Rng::new(10).bytes(mss * 5 + 77);

    sim.a.borrow_mut().send(&a_payload).unwrap();
    sim.b.borrow_mut().send(&b_payload).unwrap();

    let mut got_a = Vec::new();
    let mut got_b = Vec::new();
    for _ in 0..2000 {
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

/// 序列化的多轮请求-响应，模拟真实会话
#[tokio::test]
async fn request_response_session() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 11);
    for round in 0..8u8 {
        let req = vec![round; 200];
        sim.a.borrow_mut().send(&req).unwrap();
        let got = sim.drain_a_to_b(300).await.unwrap();
        assert_same(&got, &req);

        let resp = vec![round.wrapping_add(100); 350];
        sim.b.borrow_mut().send(&resp).unwrap();
        let got = sim.drain_b_to_a(300).await.unwrap();
        assert_same(&got, &resp);
    }
}

/// 大块数据（约 1MB）在干净链路上的完整性
#[tokio::test]
async fn large_transfer_integrity() {
    let cfg = KcpConfig::fast_mode_with_wnd(512, 512);
    let cfg = KcpConfig { stream_mode: true, ..cfg };
    let mut sim = Sim::new(cfg, Net::default(), 12);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(12).bytes(mss * 760);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    for _ in 0..20000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 数据在链路中保留顺序，没有被重排错乱
#[tokio::test]
async fn ordering_is_preserved() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 13);
    let mss = sim.a.borrow().mss as usize;
    let mut expect = Vec::new();
    for i in 0..40u32 {
        let part = Rng::new(i as u64 + 100).bytes(mss - 10);
        expect.extend_from_slice(&part);
        sim.a.borrow_mut().send(&part).unwrap();
    }

    let got = sim.drain_a_to_b(6000).await.unwrap();
    assert_same(&got, &expect);
}
