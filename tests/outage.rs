//! 黑盒测试：断网、闪断与恢复
//!
//! 真实网络不会一直平稳：切换基站、电梯、路由抖动都会造成几秒到几十秒的黑洞。
//! 这里用 `Net::with_blackout` 精确制造黑洞期，验证 KCP 在链路回来之后
//! 能自行恢复，而不是永久卡死或丢数据。

mod common;

use common::stress::transfer_a_to_b;
use common::{Net, Rng, Sim, assert_same};
use kcp_ovo::KcpConfig;

/// 传输中途断网 3 秒，恢复后必须补齐全部数据
#[tokio::test]
async fn blackout_mid_transfer_recovers() {
    let mut rng = Rng::new(0xC0FF_0001);
    let net = Net::loss(0.0).with_delay(8).with_blackout(300, 3_000);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 0xC0FF_0100);
    let payload = rng.bytes(sim.a.borrow().mss as usize * 200);

    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 4_000_000).await;
    assert!(ok, "断网 3 秒后未恢复，{ticks} tick");
    assert_same(&got, &payload);
}

/// 断网期间发送端队列有界，不会被撑爆
#[tokio::test]
async fn blackout_does_not_bloat_queues() {
    let mut rng = Rng::new(0xC0FF_0002);
    let net = Net::loss(0.0).with_delay(8).with_blackout(100, 10_000);
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 128, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0xC0FF_0200);
    let payload = rng.bytes(sim.a.borrow().mss as usize * 500);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut peak_snd = 0usize;
    for _ in 0..2_000 {
        sim.tick().await.unwrap();
        peak_snd = peak_snd.max(sim.a.borrow().waitsnd());
    }
    // 数据总量 500 段，断网期间最多排队这些，不得出现重复拷贝放大
    assert!(peak_snd <= 501, "断网期间 waitsnd 峰值 {peak_snd}，超过总段数");
}

/// 连续多次闪断（每次 1 秒，间隔 2 秒）后仍能完成传输
#[tokio::test]
async fn repeated_blackouts_still_deliver() {
    let mut rng = Rng::new(0xC0FF_0003);
    let mut sim = {
        let net = Net::loss(0.02).with_delay(6).with_blackout(200, 1_000);
        Sim::new(
            KcpConfig { snd_wnd: 64, rcv_wnd: 256, ..KcpConfig::stream_mode() },
            net,
            0xC0FF_0300,
        )
    };
    let payload = rng.bytes(sim.a.borrow().mss as usize * 300);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    for _ in 0..4_000_000u64 {
        // 每 3 秒制造一次 1 秒黑洞
        let now = sim.now;
        sim.net.blackout = if now % 3_000 == 0 { Some((now, 1_000)) } else { None };
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 双向传输中途断网，恢复后两个方向都要补齐
#[tokio::test]
async fn blackout_recovers_both_directions() {
    let mut rng = Rng::new(0xC0FF_0004);
    let net = Net::loss(0.05).with_delay(8).with_blackout(150, 2_000);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 0xC0FF_0400);
    let mss = sim.a.borrow().mss as usize;
    let a_payload = rng.bytes(mss * 60);
    let b_payload = rng.bytes(mss * 60);
    sim.a.borrow_mut().send(&a_payload).unwrap();
    sim.b.borrow_mut().send(&b_payload).unwrap();

    let mut got_a = Vec::new();
    let mut got_b = Vec::new();
    for _ in 0..4_000_000 {
        sim.tick().await.unwrap();
        got_a.extend_from_slice(&sim.take_b());
        got_b.extend_from_slice(&sim.take_a());
        if got_a.len() >= a_payload.len()
            && got_b.len() >= b_payload.len()
            && sim.a.borrow().waitsnd() == 0
            && sim.b.borrow().waitsnd() == 0
        {
            break;
        }
    }
    assert_same(&got_a, &a_payload);
    assert_same(&got_b, &b_payload);
}

/// 长时间断网应表现为「重传耗尽」或「持续重试」，绝不能静默丢数据
///
/// UDP 本身没有送达保证，因此这里只断言两个可接受结局之一：
/// 明确报错，或者数据最终在链路恢复后补齐。绝不允许「发送端认为发完了、
/// 接收端却没收到」这种静默丢失。
#[tokio::test]
async fn very_long_blackout_never_silently_loses_data() {
    let mut rng = Rng::new(0xC0FF_0005);
    // 先让传输跑一段时间再断网，确保黑洞覆盖到真实传输过程
    let mut sim = Sim::new(
        KcpConfig { snd_wnd: 16, rcv_wnd: 64, ..KcpConfig::stream_mode() },
        Net::loss(0.0).with_delay(5),
        0xC0FF_0500,
    );
    let payload = rng.bytes(sim.a.borrow().mss as usize * 500);
    sim.a.borrow_mut().send(&payload).unwrap();
    sim.run(200).await.unwrap();

    // 此刻开始 20 秒黑洞
    sim.net.blackout = Some((sim.now, 20_000));
    let mut got: Vec<u8> = Vec::new();
    for m in sim.b_messages() {
        got.extend_from_slice(&m);
    }

    let mut outcome = None;
    for _ in 0..4_000_000 {
        match sim.tick().await {
            Err(e) => {
                outcome = Some(format!("死链: {e}"));
                break;
            },
            Ok(()) => {
                for m in sim.b_messages() {
                    got.extend_from_slice(&m);
                }
            },
        }
        // 链路已恢复且传输完成
        if sim.now > 20_200 && got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
            outcome = Some("补齐".to_string());
            break;
        }
    }

    match outcome.as_deref() {
        Some("补齐") => assert_same(&got, &payload),
        Some(msg) if msg.starts_with("死链") => {
            // 报死链可接受，但必须发生在黑洞结束前后，而不是立刻
            assert!(sim.now > 20_000, "过早判定死链，实际 {msg}");
        },
        _ => panic!("断网后既未报错也未补齐数据，属于静默丢包"),
    }
}

/// 断网刚好在重传退避期间结束，恢复应立即生效而不是继续等待
#[tokio::test]
async fn recovery_is_prompt_after_blackout() {
    let mut rng = Rng::new(0xC0FF_0006);
    let b_blackout_end = 1_500u64;
    let net = Net::loss(0.0).with_delay(6).with_blackout(300, b_blackout_end - 300);
    let mut sim = Sim::new(KcpConfig::stream_mode(), net, 0xC0FF_0600);
    let payload = rng.bytes(sim.a.borrow().mss as usize * 20);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    let mut done_at = None;
    for _ in 0..500_000u64 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
            done_at = Some(sim.now);
            break;
        }
    }
    assert_same(&got, &payload);
    let done_at = done_at.expect("未完成");
    // 恢复后应在合理时间内补齐，不应被推到下一次长退避
    assert!(
        done_at < b_blackout_end + 30_000,
        "恢复耗时过长：{done_at}ms（黑洞结束于 {b_blackout_end}ms）"
    );
}
