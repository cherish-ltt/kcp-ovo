//! 黑盒测试：随机组合下的协议不变式
//!
//! 每一种链路条件都由 (seed, 参数) 完全决定，失败时用相同 seed 即可复现。
//! 断言的是与实现无关的协议契约：
//! - 不丢字节、不重字节、不乱序
//! - 消息模式下消息边界与消息内容一一对应
//! - 发送端在任何时刻都不超出对端通告的接收窗口
//! - 无论损伤多严重，都必须在有限 tick 内收敛（不永久停滞）

mod common;

use common::stress::{Rendezvous, random_net, transfer_a_to_b, transfer_both};
use common::{Net, Rng, Sim, assert_same, checksum};
use kcp_ovo::KcpConfig;

/// 覆盖多个损伤等级与多种配置的单向传输
#[tokio::test]
async fn randomized_transfer_is_byte_exact() {
    let configs: Vec<(&str, KcpConfig)> = vec![
        ("stream", KcpConfig::stream_mode()),
        ("fast", KcpConfig::fast_mode_with_wnd(32, 64)),
        ("normal", KcpConfig::fast_mode_with_wnd(8, 64)),
    ];
    let mut rng = Rng::new(0xA11C_E001);

    for (i, (tag, cfg)) in configs.iter().enumerate() {
        for round in 0..4 {
            let net = random_net(&mut rng);
            let mut sim = Sim::new(cfg.clone(), net, 0xA11C_0000 + (i * 10 + round) as u64);
            let mss = sim.a.borrow().mss as usize;
            // 1 段到 40 段的混合长度
            let payload = rng.bytes_segments(mss, 40);
            let len = payload.len();

            let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 300_000).await;
            assert!(ok, "[{}#{round}] {len} 字节未在 {ticks} tick 内送达", tag);
            assert_same(&got, &payload);
        }
    }
}

/// 双向同时流动时，两个方向的内容必须各自正确
#[tokio::test]
async fn randomized_bidirectional_is_byte_exact() {
    let mut rng = Rng::new(0xB1D1_0200);
    for round in 0..6u64 {
        let net = random_net(&mut rng);
        let cfg = KcpConfig::fast_mode_with_wnd(32, 64);
        let mut sim = Sim::new(cfg, net, 0xB1D1_0000 + round);
        let mss = sim.a.borrow().mss as usize;
        let a_payload = rng.bytes_segments(mss, 20);
        let b_payload = rng.bytes_segments(mss, 20);

        let (ok, got_a, got_b, ticks) =
            transfer_both(&mut sim, &a_payload, &b_payload, 400_000).await;
        assert!(ok, "[#{round}] 双向传输未在 {ticks} tick 内完成");
        assert_same(&got_a, &a_payload);
        assert_same(&got_b, &b_payload);
    }
}

/// 消息模式下每条消息的边界与内容必须原样保留
#[tokio::test]
async fn randomized_message_boundaries_are_preserved() {
    for round in 0..4u64 {
        let net = Net::loss(0.2).with_delay(4).with_jitter(4).with_reorder(0.2, 0.1);
        // rcv_wnd 取大一点，保证单条消息的分片数不超过窗口
        let cfg = KcpConfig { rcv_wnd: 128, ..KcpConfig::fast_mode_with_wnd(32, 128) };
        // 每轮独立种子：否则各轮生成的数据完全相同，等于只测了一份样本
        let mut rng = Rng::new(0xE9E1_0300 + round);
        let mut sim = Sim::new(cfg, net, 0xE9E1_0000 + round);
        let mss = sim.a.borrow().mss as usize;

        let mut sent: Vec<Vec<u8>> = Vec::new();
        let mut total = 0usize;
        for _ in 0..8 {
            // 每条 1..=6 段
            let msg = rng.bytes_segments(mss, 6);
            total += msg.len();
            sim.a.borrow_mut().send(&msg).unwrap();
            sent.push(msg);
        }

        let mut rdv = Rendezvous::new(&mut sim, total, 0);
        assert!(rdv.run(400_000).await, "[#{round}] 消息未全部送达");
        // 消息边界必须在逐次 recv 时就核对：字节流拼接后无法还原边界
        let got = rdv.messages_a_to_b().to_vec();
        assert_eq!(got.len(), sent.len(), "[#{round}] 消息条数不一致");
        for (i, (g, s)) in got.iter().zip(sent.iter()).enumerate() {
            assert_eq!(g.len(), s.len(), "[#{round}] 第 {i} 条消息长度不一致");
            assert_eq!(checksum(g), checksum(s), "[#{round}] 第 {i} 条消息内容不一致");
        }
    }
}

/// 任意时刻在途段数不得超过对端通告的接收窗口
#[tokio::test]
async fn sender_never_exceeds_peer_window() {
    for round in 0..4u64 {
        let cfg = KcpConfig::fast_mode_with_wnd(64, 64);
        let net = Net::loss(0.15).with_delay(6).with_jitter(4);
        let mut rng = Rng::new(0xC0DE_0400 + round);
        let mut sim = Sim::new(cfg, net, 0xC0DE_0000 + round);
        let mss = sim.a.borrow().mss as usize;
        let payload = rng.bytes(mss * 120);
        sim.a.borrow_mut().send(&payload).unwrap();

        for _ in 0..2000 {
            let before = sim.a.borrow().snd_buf.len();
            sim.tick().await.unwrap();
            let after = sim.a.borrow().snd_buf.len();
            // 只有本轮新推出的段受窗口约束。已发出但还没被确认的段
            // 即使在途窗口收缩也不会从 snd_buf 移除（与 C 原版一致），
            // 因此这里断言的是“每次搬移不超窗”，而非“总量不超窗”。
            if sim.a.borrow().peer_seen {
                let wnd = sim.a.borrow().rmt_wnd as usize;
                let admitted = after.saturating_sub(before);
                assert!(
                    admitted <= wnd,
                    "[#{round}] 本轮新推出 {admitted} 段，超过对端通告窗口 {wnd}"
                );
            }
            sim.take_b();
        }
        // 发送窗口约束的是“已发出未确认”的段数；snd_buffer 整体可能更大，
        // 因为 snd_queue 里排队等窗口的段不占在途额度。
        assert!(
            sim.a.borrow().snd_buf.len() <= 64,
            "[#{round}] 结束时仍有 {} 段在途，超过发送窗口",
            sim.a.borrow().snd_buf.len()
        );
    }
}

/// 接收端的缓存与队列不得超出接收窗口
#[tokio::test]
async fn receiver_never_exceeds_own_window() {
    let mut rng = Rng::new(0x9EEC_0500);
    for round in 0..4u64 {
        // 接收窗口刻意设小，且发送端窗口更大
        let cfg = KcpConfig::fast_mode_with_wnd(4, 4);
        let net = Net::loss(0.1).with_delay(5).with_jitter(3).with_reorder(0.3, 0.2);
        let mut sim = Sim::new(cfg, net, 0x9EEC_0000 + round);
        let mss = sim.a.borrow().mss as usize;
        // 不读数据，持续给压力
        sim.a.borrow_mut().send(&rng.bytes(mss * 40)).unwrap();

        for _ in 0..1500 {
            sim.tick().await.unwrap();
            let b = sim.b.borrow();
            assert!(
                b.nrcv_buf <= b.rcv_wnd as usize,
                "[#{round}] rcv_buf {} 超过接收窗口 {}",
                b.nrcv_buf,
                b.rcv_wnd
            );
            assert!(
                b.nrcv_que <= b.rcv_wnd as usize,
                "[#{round}] rcv_queue {} 超过接收窗口 {}",
                b.nrcv_que,
                b.rcv_wnd
            );
        }
    }
}

/// 长时间会话：连续多轮收发后不残留待发段，且累计内容正确
#[tokio::test]
async fn long_session_stays_consistent() {
    let mut rng = Rng::new(0x10A6_0600);
    let net = Net::loss(0.08).with_delay(3).with_jitter(2);
    let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(16, 64), net, 0x10A6_0601);
    let mss = sim.a.borrow().mss as usize;

    let mut all = Vec::new();
    for round in 0..40u64 {
        let msg = rng.bytes_segments(mss, 5);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &msg, 200_000).await;
        assert!(ok, "[#{round}] 第 {round} 轮未送达，耗时 {ticks} tick");
        assert_same(&got, &msg);
        all.extend_from_slice(&msg);
    }
    assert_eq!(sim.a.borrow().waitsnd(), 0, "会话结束后不应残留待发段");
    assert_same(&sim.take_b(), &[]);
    assert!(all.len() > mss);
}

/// 极端丢包下仍应收敛（不保证快，但不能永久停滞）
#[tokio::test]
async fn extreme_loss_still_converges() {
    let mut rng = Rng::new(0x71E5_0700);
    for round in 0..3u64 {
        let net = Net::loss(0.5).with_delay(8);
        let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(8, 32), net, 0x71E5_0000 + round);
        let payload = rng.bytes(sim.a.borrow().mss as usize * 6);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 600_000).await;
        assert!(ok, "[#{round}] 50% 丢包下未收敛，{ticks} tick");
        assert_same(&got, &payload);
    }
}

/// 延迟骤增（模拟长肥管道）时依然完整
#[tokio::test]
async fn high_latency_link_still_delivers() {
    let mut rng = Rng::new(0x1A7E_0800);
    for delay in [60u64, 150, 400] {
        let net = Net::loss(0.05).with_delay(delay);
        let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(16, 64), net, 0x1A7E_0000 + delay);
        let payload = rng.bytes(sim.a.borrow().mss as usize * 8);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 800_000).await;
        assert!(ok, "延迟 {delay}ms 时未收敛，{ticks} tick");
        assert_same(&got, &payload);
    }
}

/// 链路中途整体断开再恢复，数据最终仍需全部到达
#[tokio::test]
async fn link_outage_and_recovery() {
    let mut rng = Rng::new(0x0A7A_0900u64);
    let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(8, 32), Net::default(), 0x0A7A_0901);
    let payload = rng.bytes(sim.a.borrow().mss as usize * 20);
    sim.a.borrow_mut().send(&payload).unwrap();

    // 前 50 tick 正常
    sim.run(50).await.unwrap();
    sim.take_b();

    // 断开 800ms：全部丢包
    sim.net.loss = 1.0;
    sim.run(800).await.unwrap();
    sim.take_b();

    // 恢复
    sim.net.loss = 0.05;
    let mut rdv = Rendezvous::new(&mut sim, payload.len(), 0);
    assert!(rdv.run(600_000).await, "链路恢复后未能补齐数据");
    assert_same(rdv.received_a_to_b(), &payload);
}
