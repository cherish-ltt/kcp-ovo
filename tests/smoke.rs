//! 黑盒冒烟测试：全部使用虚拟时钟，秒级跑完，只验「连通、不崩、不丢」
//!
//! 与其它套件的分工：这里不追求场景深度，而是用最少的 tick 覆盖尽可能多的
//! 「配置 × 方向 × 数据量」入口，作为每次改动后的第一道快速反馈。
//! 任何一条失败都意味着代码根本不可用，无需再跑其余套件。

#![allow(clippy::await_holding_refcell_ref)]

mod common;

use common::stress::{RawHeader, Rendezvous, config_matrix, control_packet, raw_packet};
use common::{Net, Rng, Sim, Solo, assert_same, checksum, parse_cmd};
use kcp_ovo::KcpConfig;
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::KcpCmd;

/// 每种预设配置都能把一条消息完整送到对端
///
/// 冒烟的第一层：任一预设配置连一条消息都送不到，其余测试都无意义。
#[tokio::test]
async fn every_preset_delivers_one_message() {
    let presets: Vec<(&str, KcpConfig)> = vec![
        ("default", KcpConfig::default()),
        ("normal_mode", KcpConfig::normal_mode()),
        ("fast_mode", KcpConfig::fast_mode()),
        ("stream_mode", KcpConfig::stream_mode()),
        ("fast_mode_with_wnd", KcpConfig::fast_mode_with_wnd(16, 32)),
    ];
    for (name, cfg) in presets {
        let mut sim = Sim::new(cfg, Net::default(), 0x5_0001);
        let payload = b"smoke".to_vec();
        sim.a.borrow_mut().send(&payload).unwrap();
        let mut rdv = Rendezvous::new(&mut sim, payload.len(), 0);
        assert!(rdv.run(20_000).await, "[{name}] 一条消息都送不到");
        assert_same(rdv.received_a_to_b(), &payload);
    }
}

/// 配置矩阵里每一组都能双向互连
#[tokio::test]
async fn config_matrix_all_connect() {
    for case in config_matrix() {
        let mut sim = Sim::asymmetric(case.a, case.b, Net::default(), 0x5_0002);
        let a_msg = vec![0xA1u8; 300];
        let b_msg = vec![0xB2u8; 300];
        sim.a.borrow_mut().send(&a_msg).unwrap();
        sim.b.borrow_mut().send(&b_msg).unwrap();
        let mut rdv = Rendezvous::new(&mut sim, a_msg.len(), b_msg.len());
        assert!(rdv.run(20_000).await, "[{}] 双向互连失败", case.name);
        assert_same(rdv.received_a_to_b(), &a_msg);
        assert_same(rdv.received_b_to_a(), &b_msg);
    }
}

/// 空数据与零长度输入不会破坏状态机
#[tokio::test]
async fn empty_inputs_are_noops() {
    let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    assert_eq!(kcp.send(b"").unwrap(), 0, "空数据应发送 0 字节");
    assert_eq!(kcp.waitsnd(), 0, "空数据不得进入发送队列");
    assert!(kcp.recv().is_err(), "空队列 recv 应报错");
    assert!(kcp.peeksize().is_err(), "空队列 peeksize 应报错");
    assert!(kcp.input(&[]).is_err(), "空包应报错");
}

/// 单端在没有任何对端响应时也只放行一段探路，不会把队列灌爆
#[tokio::test]
async fn lone_sender_admits_one_probe_segment() {
    let mut solo = Solo::with_conv(1, KcpConfig::fast_mode_with_wnd(64, 128));
    let _ = solo.kcp.borrow_mut().send(&vec![1u8; 20_000]);
    solo.step(20).await.unwrap();
    assert_eq!(solo.kcp.borrow().nsnd_buf, 1, "未知对端时只应放行 1 段探路");
    // 多轮 flush 也不应再放行
    for _ in 0..20 {
        solo.step(20).await.unwrap();
    }
    assert_eq!(solo.kcp.borrow().nsnd_buf, 1, "对端未响应时不得继续放行新段");
}

/// 收到对端窗口通告后立即恢复正常发送
#[tokio::test]
async fn peer_window_notice_unblocks_sender() {
    let mut solo = Solo::with_conv(1, KcpConfig::fast_mode_with_wnd(8, 16));
    solo.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 0)).unwrap();
    solo.kcp.borrow_mut().send(&vec![1u8; 5_000]).unwrap();
    solo.step(20).await.unwrap();
    assert_eq!(solo.kcp.borrow().nsnd_buf, 0, "对端窗口为 0 时不得发送数据");

    solo.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 16)).unwrap();
    solo.step(40).await.unwrap();
    assert!(solo.kcp.borrow().nsnd_buf > 0, "收到非零窗口后应恢复发送");
}

/// 所有控制帧往返都不会 panic，且 conv 不匹配时静默丢弃
#[tokio::test]
async fn control_frames_roundtrip_without_panic() {
    let solo = Solo::with_conv(0x1234, KcpConfig::fast_mode_with_wnd(8, 16));
    for cmd in [KcpCmd::Wask, KcpCmd::Wins] {
        let pkt = control_packet(0x1234, cmd, 16);
        assert!(parse_cmd(&pkt) == Some(cmd));
        solo.kcp.borrow_mut().input(&pkt).unwrap();
    }
    // 异 conv 的包必须被静默丢弃
    let foreign = raw_packet(RawHeader::push(0x9999, 0), b"evil");
    assert!(solo.kcp.borrow_mut().input(&foreign).is_ok());
    assert_eq!(solo.kcp.borrow().nrcv_que, 0, "异 conv 包不得进入接收队列");
}

/// 双向同时发送互不干扰，字节精确
#[tokio::test]
async fn simultaneous_bidirectional_transfer() {
    let mut rng = Rng::new(0x5_0003);
    for round in 0..3u64 {
        let cfg = KcpConfig::stream_mode();
        let mut sim = Sim::new(cfg, Net::loss(0.05).with_delay(8), 0x5_0100 + round);
        let a_msg = rng.bytes(8_000);
        let b_msg = rng.bytes(8_000);
        sim.a.borrow_mut().send(&a_msg).unwrap();
        sim.b.borrow_mut().send(&b_msg).unwrap();

        let mut rdv = Rendezvous::new(&mut sim, a_msg.len(), b_msg.len());
        assert!(rdv.run(50_000).await, "[#{round}] 双向传输未完成");
        assert_eq!(checksum(rdv.received_a_to_b()), checksum(&a_msg), "[#{round}] A→B 内容不符");
        assert_eq!(checksum(rdv.received_b_to_a()), checksum(&b_msg), "[#{round}] B→A 内容不符");
    }
}

/// 连续小消息在单连接上都能送达，消息边界不混淆
#[tokio::test]
async fn many_small_messages_keep_boundaries() {
    let cfg = KcpConfig::fast_mode_with_wnd(16, 32);
    let mut sim = Sim::new(cfg, Net::loss(0.02).with_delay(5), 0x5_0004);
    let msgs: Vec<Vec<u8>> = (0..30u8).map(|i| vec![i; 40 + i as usize]).collect();
    let total: usize = msgs.iter().map(|m| m.len()).sum();
    for m in &msgs {
        sim.a.borrow_mut().send(m).unwrap();
    }
    let mut rdv = Rendezvous::new(&mut sim, total, 0);
    assert!(rdv.run(50_000).await, "小消息流未送达");
    let got = rdv.messages_a_to_b();
    assert_eq!(got.len(), msgs.len(), "消息条数不一致");
    for (i, (a, b)) in got.iter().zip(&msgs).enumerate() {
        assert_same(a, b);
        assert_eq!(a.len(), b.len(), "第 {i} 条消息长度不符");
    }
}

/// 构造期的退化参数一律拒绝，不允许静默创建不可用连接
#[tokio::test]
async fn degenerate_configs_rejected() {
    let bad: Vec<(&str, KcpConfig)> = vec![
        ("mtu 过小", KcpConfig { mtu: 20, ..KcpConfig::fast_mode() }),
        ("mtu 恰为包头", KcpConfig { mtu: 28, ..KcpConfig::fast_mode() }),
        ("snd_wnd 为 0", KcpConfig { snd_wnd: 0, ..KcpConfig::fast_mode() }),
        ("rcv_wnd 为 0", KcpConfig { rcv_wnd: 0, ..KcpConfig::fast_mode() }),
    ];
    for (name, cfg) in bad {
        assert!(Kcp::new(1, cfg).is_err(), "[{name}] 非法配置应被拒绝");
    }
}

/// 最小可用配置（mtu=29）能完成一次真实传输
#[tokio::test]
async fn minimum_config_transfers() {
    let cfg = KcpConfig { mtu: 29, snd_wnd: 8, rcv_wnd: 16, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, Net::default(), 0x5_0005);
    let payload = vec![0x7Eu8; 200];
    sim.a.borrow_mut().send(&payload).unwrap();
    let mut rdv = Rendezvous::new(&mut sim, payload.len(), 0);
    assert!(rdv.run(80_000).await, "mtu=29 的最小配置无法传输");
    assert_same(rdv.received_a_to_b(), &payload);
}

/// 每次 update 都遵守 flush 间隔，不会同一时刻重复刷新
#[tokio::test]
async fn update_respects_flush_interval() {
    let mut solo = Solo::with_conv(1, KcpConfig::fast_mode_with_wnd(8, 16));
    solo.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 16)).unwrap();
    solo.kcp.borrow_mut().send(&[1u8; 2_000]).unwrap();

    solo.step(20).await.unwrap();
    let first = solo.take_out().len();
    assert!(first > 0, "首轮 flush 应发出数据");

    // 同一时刻重复 update 不应再发
    let now = solo.now;
    solo.kcp.borrow_mut().update(now).await.unwrap();
    assert_eq!(solo.take_out().len(), first, "同一时刻重复 update 不应重复发包");
}
