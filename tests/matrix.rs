//! 黑盒测试：配置组合矩阵
//!
//! 单个配置正确不代表组合正确。这里把 MTU、窗口、拥塞控制、重传策略、
//! 更新间隔、流/消息模式两两交叉，验证任意组合下都能完成传输。

mod common;

use common::stress::{config_matrix, transfer_a_to_b, transfer_both};
use common::{Net, Rng, Sim};
use kcp_ovo::KcpConfig;

/// 所有配置组合在干净链路上都应完成传输
#[tokio::test]
async fn all_configs_interoperate_clean() {
    for case in config_matrix() {
        let mut rng = Rng::new(0xCAFE_0001);
        let mut sim = Sim::asymmetric(case.a, case.b, Net::default(), 0xCAFE_0100);
        let mss = sim.a.borrow().mss as usize;
        // 保持 6 段以内，确保消息模式下整条消息能落在接收窗口里
        let payload = rng.bytes_segments(mss, 3);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 200_000).await;
        assert!(ok, "[{}] 干净链路未完成，{ticks} tick", case.name);
        common::assert_same(&got, &payload);
    }
}

/// 所有配置组合在有损链路上都应完成传输
#[tokio::test]
async fn all_configs_interoperate_lossy() {
    for case in config_matrix() {
        let mut rng = Rng::new(0xCAFE_0002);
        let net = Net::loss(0.15).with_delay(6).with_jitter(4).with_reorder(0.15, 0.1);
        let mut sim = Sim::asymmetric(case.a, case.b, net, 0xCAFE_0200);
        let mss = sim.a.borrow().mss as usize;
        let payload = rng.bytes_segments(mss, 3);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 600_000).await;
        assert!(ok, "[{}] 有损链路未完成，{ticks} tick", case.name);
        common::assert_same(&got, &payload);
    }
}

/// 所有配置组合的双向传输都应完成
#[tokio::test]
async fn all_configs_bidirectional() {
    for case in config_matrix() {
        let mut rng = Rng::new(0xCAFE_0003);
        let net = Net::loss(0.1).with_delay(5);
        let mut sim = Sim::asymmetric(case.a, case.b, net, 0xCAFE_0300);
        let mss = sim.a.borrow().mss as usize;
        let a_payload = rng.bytes_segments(mss, 3);
        let b_payload = rng.bytes_segments(mss, 3);
        let (ok, got_a, got_b, ticks) =
            transfer_both(&mut sim, &a_payload, &b_payload, 600_000).await;
        assert!(ok, "[{}] 双向未完成，{ticks} tick", case.name);
        common::assert_same(&got_a, &a_payload);
        common::assert_same(&got_b, &b_payload);
    }
}

/// MTU 越小分片越多，但内容必须一致
#[tokio::test]
async fn mtu_variations_preserve_content() {
    for mtu in [128u32, 200, 576, 1024, 1400, 1500] {
        let cfg = KcpConfig { mtu, ..KcpConfig::stream_mode() };
        let mut sim = Sim::new(cfg, Net::default(), 0xCAFE_0400 + mtu as u64);
        let payload = Rng::new(mtu as u64).bytes((mtu as usize - 28) * 4 + 7);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 200_000).await;
        assert!(ok, "mtu={mtu} 未完成，{ticks} tick");
        common::assert_same(&got, &payload);
    }
}

/// 更新间隔决定反应速度，但不影响最终完整性
#[tokio::test]
async fn interval_variations_still_complete() {
    for interval in [1u64, 10, 20, 50, 100, 200] {
        let cfg = KcpConfig { interval, ..KcpConfig::fast_mode_with_wnd(8, 32) };
        let mut sim = Sim::new(cfg, Net::loss(0.1).with_delay(4), 0xCAFE_0500 + interval);
        let payload = Rng::new(interval).bytes(sim.a.borrow().mss as usize * 4);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 900_000).await;
        assert!(ok, "interval={interval} 未完成，{ticks} tick");
        common::assert_same(&got, &payload);
    }
}

/// fastresend 取值不同只影响速度，不影响正确性
#[tokio::test]
async fn fastresend_variations_are_correct() {
    for fastresend in [0i32, 1, 2, 3, 5] {
        let cfg = KcpConfig { fastresend, ..KcpConfig::fast_mode_with_wnd(16, 64) };
        let mut sim = Sim::new(cfg, Net::loss(0.2).with_delay(5), 0xCAFE_0600 + fastresend as u64);
        let payload = Rng::new(fastresend as u64 + 1).bytes(sim.a.borrow().mss as usize * 10);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 900_000).await;
        assert!(ok, "fastresend={fastresend} 未完成，{ticks} tick");
        common::assert_same(&got, &payload);
    }
}

/// nodelay 开关只影响聚合行为，不影响交付内容
#[tokio::test]
async fn nodelay_toggle_is_transparent() {
    for nodelay in [true, false] {
        for nocwnd in [true, false] {
            let cfg = KcpConfig { nodelay, nocwnd, ..KcpConfig::fast_mode_with_wnd(16, 64) };
            let mut sim = Sim::new(cfg, Net::loss(0.05).with_delay(4), 0xCAFE_0700);
            let payload = Rng::new(7).bytes(sim.a.borrow().mss as usize * 5);
            let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 300_000).await;
            assert!(ok, "nodelay={nodelay} nocwnd={nocwnd} 未完成，{ticks} tick");
            common::assert_same(&got, &payload);
        }
    }
}

/// 发送窗口为 1 时逐段停等，速度慢但必须正确
#[tokio::test]
async fn single_segment_window_works() {
    let cfg = KcpConfig { snd_wnd: 1, rcv_wnd: 16, ..KcpConfig::fast_mode() };
    let mut sim = Sim::new(cfg, Net::loss(0.05).with_delay(5), 0xCAFE_0800);
    let payload = Rng::new(8).bytes(sim.a.borrow().mss as usize * 6);
    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 900_000).await;
    assert!(ok, "窗口为 1 时未完成，{ticks} tick");
    common::assert_same(&got, &payload);
}

/// 收发窗口悬殊（1 vs 512）时应由小窗口一侧限速，且不丢数据
///
/// 用流模式：消息模式下 rcv_queue 满即通告零窗口，本用例的 20 段单条消息
/// 会先把 rcv_wnd=16 填满，属于协议语义而非缺陷。
#[tokio::test]
async fn lopsided_windows_transfer_correctly() {
    let a = KcpConfig { ..KcpConfig::fast_mode_with_wnd(1, 512) };
    let b = KcpConfig { rcv_wnd: 16, stream_mode: true, ..KcpConfig::fast_mode_with_wnd(512, 16) };
    let a = KcpConfig { stream_mode: true, ..a };
    let mut sim = Sim::asymmetric(a, b, Net::default(), 0xCAFE_0900);
    let payload = Rng::new(9).bytes(sim.a.borrow().mss as usize * 20);
    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 900_000).await;
    assert!(ok, "悬殊窗口未完成，{ticks} tick");
    common::assert_same(&got, &payload);
    assert!(sim.a.borrow().snd_buf.len() <= 1, "发送端在途段数超过自身窗口 1");
}
