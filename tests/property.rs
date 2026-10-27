//! 黑盒测试：属性测试
//!
//! 与其它用例的固定种子不同，这里用 proptest 在参数空间里自动搜索并收缩反例：
//! 输入是任意字节、任意头部字段、任意配置、任意链路参数。失败时 proptest 会打印
//! 最小反例并写入 `proptest-regressions/`，该目录必须随修复一起提交，
//! 使同一反例在后续每轮 CI 中都会重跑。

mod common;

use common::stress::RawHeader;
use common::stress::raw_packet;
use common::{Net, Rng, Sim, assert_same};
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::{KcpCmd, KcpConfig, KcpError};
use proptest::prelude::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build runtime")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// 任意字节序列都不得让解析路径崩溃或撑爆接收队列
    #[test]
    fn arbitrary_datagram_never_breaks_state(
        data in prop::collection::vec(any::<u8>(), 0..600),
        repeat in 1usize..8,
    ) {
        let cfg = KcpConfig { rcv_wnd: 8, ..KcpConfig::stream_mode() };
        let mut kcp = Kcp::new(1, cfg).expect("config should be valid");
        for _ in 0..repeat {
            let _ = kcp.input(&data);
        }
        prop_assert!(kcp.nrcv_buf <= kcp.rcv_wnd as usize, "rcv_buf 越界");
        prop_assert!(kcp.nrcv_que <= kcp.rcv_wnd as usize, "rcv_queue 越界");
        prop_assert!(kcp.snd_buf.len() <= kcp.snd_wnd as usize, "snd_buf 越界");
        prop_assert!(kcp.mss > 0);
    }

    /// 任意头部字段组合都不得让解析路径崩溃
    #[test]
    fn arbitrary_header_never_breaks_state(
        cmd in 81u8..=84,
        frg in any::<u8>(),
        wnd in any::<u16>(),
        ts in any::<u64>(),
        sn in any::<u32>(),
        una in any::<u32>(),
        declared_len in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let cfg = KcpConfig { rcv_wnd: 16, ..KcpConfig::stream_mode() };
        let mut kcp = Kcp::new(1, cfg).expect("config should be valid");

        // 头部长度字段故意与真实载荷不一致：解析器必须以错误收场，不能越界读
        let packet = raw_packet(
            RawHeader { conv: 1, cmd, frg, wnd, ts, sn, una },
            &payload,
        );
        let mut packet = packet;
        packet[24..28].copy_from_slice(&declared_len.to_be_bytes());
        let _ = kcp.input(&packet);

        let _ = KcpCmd::try_from(cmd).expect("cmd range is valid");
        prop_assert!(kcp.nrcv_buf <= kcp.rcv_wnd as usize);
        prop_assert!(kcp.nrcv_que <= kcp.rcv_wnd as usize);
    }

    /// 任意配置要么在构造期被明确拒绝，要么必须是可用的
    #[test]
    fn arbitrary_config_is_rejected_or_usable(
        mtu in 0u32..4000,
        snd_wnd in any::<u16>(),
        rcv_wnd in any::<u16>(),
        interval in 0u64..500,
        fastresend in 0i32..6,
        nodelay in any::<bool>(),
        stream_mode in any::<bool>(),
    ) {
        let cfg = KcpConfig { mtu, interval, nodelay, fastresend, nocwnd: true, snd_wnd, rcv_wnd, stream_mode };
        match Kcp::new(7, cfg) {
            Err(err) => prop_assert!(matches!(err, KcpError::InvalidConfig(_))),
            Ok(mut kcp) => {
                prop_assert!(kcp.mss > 0, "可用配置的 mss 必须为正");
                prop_assert!(kcp.snd_wnd > 0 && kcp.rcv_wnd > 0);
                // 发送路径不得 panic：分片数受 255 上限约束，非流模式超限应报错
                let data = vec![0xABu8; 4096];
                let sent = kcp.send(&data);
                prop_assert!(sent.is_ok() || !stream_mode);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, max_shrink_iters: 64, ..ProptestConfig::default() })]

    /// 任意链路参数下，只要 RTO 有时间收敛，字节流就必须完整且顺序不变
    #[test]
    fn random_link_delivers_byte_exact(
        loss in 0.0f64..0.45,
        delay in 1u64..60,
        jitter in 0u64..20,
        reorder in 0.0f64..0.5,
        duplicate in 0.0f64..0.3,
        segments in 1usize..24,
    ) {
        let rt = runtime();
        let result: Result<(), TestCaseError> = rt.block_on(async move {
            let net = Net { loss, delay, jitter, reorder, duplicate, ..Net::default() };
            let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 64, ..KcpConfig::stream_mode() };
            let mut sim = Sim::new(cfg, net, 0xF00D);
            let mss = sim.a.borrow().mss as usize;
            let payload = Rng::new(0xF00D_0001).bytes(mss * segments);
            sim.a.borrow_mut().send(&payload).expect("send");

            let mut got = Vec::new();
            for _ in 0..600_000 {
                sim.tick().await.expect("update");
                got.extend_from_slice(&sim.take_b());
                if got.len() >= payload.len() {
                    break;
                }
            }
            prop_assert_eq!(got.len(), payload.len(), "未在 tick 上限内送达全部字节");
            assert_same(&got, &payload);
            Ok(())
        });
        result?;
    }

    /// 双向同时传输在任意链路参数下都不得串包或丢包
    #[test]
    fn random_link_bidirectional_is_byte_exact(
        loss in 0.0f64..0.35,
        delay in 1u64..40,
        jitter in 0u64..12,
        reorder in 0.0f64..0.4,
        duplicate in 0.0f64..0.25,
        segments in 1usize..12,
    ) {
        let rt = runtime();
        let result: Result<(), TestCaseError> = rt.block_on(async move {
            let net = Net { loss, delay, jitter, reorder, duplicate, ..Net::default() };
            let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 64, ..KcpConfig::stream_mode() };
            let mut sim = Sim::new(cfg, net, 0xBEEF);
            let mss = sim.a.borrow().mss as usize;
            let mut rng = Rng::new(0xBEEF_0002);
            let a_payload = rng.bytes(mss * segments);
            let b_payload = rng.bytes(mss * segments);
            sim.a.borrow_mut().send(&a_payload).expect("send a");
            sim.b.borrow_mut().send(&b_payload).expect("send b");

            let mut got_a = Vec::new();
            let mut got_b = Vec::new();
            for _ in 0..600_000 {
                sim.tick().await.expect("update");
                got_a.extend_from_slice(&sim.take_a());
                got_b.extend_from_slice(&sim.take_b());
                if got_a.len() >= b_payload.len() && got_b.len() >= a_payload.len() {
                    break;
                }
            }
            assert_same(&got_b, &a_payload);
            assert_same(&got_a, &b_payload);
            Ok(())
        });
        result?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, max_shrink_iters: 64, ..ProptestConfig::default() })]

    /// 接收窗口很小时，在途段数仍不得超过对端容量，且字节流必须完整
    ///
    /// 窗口越小越容易暴露「每轮 flush 都按整个窗口放行」的记账错误：
    /// 在途量会随 flush 次数不断叠加，直到接收端整片丢弃。
    #[test]
    fn small_receive_window_bounds_in_flight(
        rcv_wnd in 1u16..8,
        loss in 0.0f64..0.3,
        delay in 1u64..40,
        segments in 4usize..20,
    ) {
        let rt = runtime();
        let result: Result<(), TestCaseError> = rt.block_on(async move {
            let net = Net { loss, delay, ..Net::default() };
            let cfg_a = KcpConfig { snd_wnd: 32, rcv_wnd: 32, ..KcpConfig::stream_mode() };
            let cfg_b = KcpConfig { rcv_wnd, ..cfg_a };
            let mut sim = Sim::asymmetric(cfg_a, cfg_b, net, 0x5A11);
            let mss = sim.a.borrow().mss as usize;
            let payload = Rng::new(0x5A11_0003).bytes(mss * segments);
            sim.a.borrow_mut().send(&payload).expect("send");

            let peer_capacity = rcv_wnd as usize;
            let mut got = Vec::new();
            for _ in 0..600_000 {
                sim.tick().await.expect("update");
                prop_assert!(
                    sim.a.borrow().snd_buf.len() <= peer_capacity,
                    "在途段数越过对端接收窗口"
                );
                got.extend_from_slice(&sim.take_b());
                if got.len() >= payload.len() {
                    break;
                }
            }
            assert_same(&got, &payload);
            Ok(())
        });
        result?;
    }
}
