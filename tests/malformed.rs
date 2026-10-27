//! 黑盒测试：畸形包与恶意输入
//!
//! 网络对端可以是任意实现，也可能是攻击者。这里关心的不是“能否正确解析”，
//! 而是最低底线：不 panic、不无界增长、不把坏数据当正常数据交付。

mod common;

use common::stress::{RawHeader, Sink, control_packet, push_packet, raw_packet};
use common::{Rng, checksum};
use kcp_ovo::KcpConfig;
use kcp_ovo::error::KcpErrorKind;
use kcp_ovo::helper::KcpCmd;

/// 任意长度的随机字节都不得让 input 崩溃
#[tokio::test]
async fn random_bytes_never_panic() {
    let mut rng = Rng::new(0xBAD0_0001);
    let mut sink = Sink::new(KcpConfig::fast_mode());
    for len in [0usize, 1, 4, 27, 28, 29, 64, 512, 1400, 4096] {
        for _ in 0..40 {
            let junk = rng.bytes(len);
            // 允许返回错误，但不得 panic
            let _ = sink.kcp.borrow_mut().input(&junk);
        }
    }
    // 之后仍应正常工作
    sink.kcp.borrow_mut().send(b"still alive").unwrap();
    sink.update(100).await.unwrap();
    assert!(!sink.emitted().is_empty(), "异常输入之后仍应能正常发送");
}

/// 头部被截断时应返回明确的错误类型
#[test]
fn truncated_header_reports_kind() {
    let sink = Sink::new(KcpConfig::default());
    let err = sink.kcp.borrow_mut().input(&[0u8; 27]).unwrap_err();
    assert_eq!(err.kind(), KcpErrorKind::IncompleteData);
}

/// 声明长度大于实际载荷时必须拒绝，不能把残缺数据当完整段收下
#[test]
fn declared_length_beyond_payload_rejected() {
    let sink = Sink::new(KcpConfig::default());
    let mut pkt = push_packet(1, 0, b"0123456789");
    // 把 len 改成 100，实际只有 10 字节
    pkt[24..28].copy_from_slice(&100u32.to_be_bytes());
    let err = sink.kcp.borrow_mut().input(&pkt).unwrap_err();
    assert_eq!(err.kind(), KcpErrorKind::IncompleteData);
    assert!(sink.kcp.borrow_mut().recv().is_err());
}

/// 未知命令字必须被当作错误，而不是静默接受
#[test]
fn unknown_command_rejected() {
    let sink = Sink::new(KcpConfig::default());
    for cmd in [0u8, 1, 80, 85, 200, 255] {
        let pkt = raw_packet(RawHeader { conv: 1, cmd, wnd: 1024, ..RawHeader::default() }, b"");
        let err = sink.kcp.borrow_mut().input(&pkt).unwrap_err();
        assert_eq!(err.kind(), KcpErrorKind::InvalidCommand, "cmd={cmd}");
    }
}

/// 伪造的 WASK 洪水：只应触发有限次 WINS 回复，不能撑爆发送队列
#[tokio::test]
async fn wask_flood_is_bounded() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    for _ in 0..10_000 {
        sink.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wask, 512)).unwrap();
    }
    // probe 只是位标记，重复 WASK 不应累积成多次回复
    sink.update(50).await.unwrap();
    let wins = sink.count_cmd(KcpCmd::Wins);
    assert!(wins <= 1, "10k 次 WASK 产生了 {wins} 个 WINS，疑似无界排队");
}

/// 伪造的 WINS 洪水不应造成任何状态破坏
#[tokio::test]
async fn wins_flood_is_harmless() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    for i in 0..10_000u32 {
        sink.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, (i % 65536) as u16)).unwrap();
    }
    sink.update(50).await.unwrap();
    assert!(sink.count_cmd(KcpCmd::Wins) <= 1);
    assert_eq!(sink.kcp.borrow().waitsnd(), 0);
}

/// 窗口外的高 SN 数据段必须被丢弃，且不占用接收缓冲区
#[test]
fn out_of_window_data_dropped() {
    let sink = Sink::new(KcpConfig { rcv_wnd: 8, ..KcpConfig::default() });
    // rcv_nxt=0，窗口 8；sn=1000 在窗口外
    for sn in [8u32, 100, 1000, u32::MAX] {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, b"far away")).unwrap();
    }
    assert_eq!(sink.kcp.borrow().nrcv_buf, 0, "窗口外的段不应进入 rcv_buf");
}

/// 旧 SN 数据段不应被当作新数据交付
#[tokio::test]
async fn stale_sn_not_redelivered() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    for sn in 0..5u32 {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, b"abc")).unwrap();
    }
    let first = sink.kcp.borrow_mut().recv().unwrap();
    assert_eq!(first.len(), 15);

    // 重放旧段
    for sn in 0..5u32 {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, b"abc")).unwrap();
    }
    sink.update(100).await.unwrap();
    assert!(sink.kcp.borrow_mut().recv().is_err(), "旧段被重复交付");
}

/// 恶意 frg 链：宣称还有 255 个分片但永远不补齐，不得无限等待内存增长
#[tokio::test]
async fn bogus_frg_chain_does_not_grow() {
    let mut sink = Sink::new(KcpConfig { rcv_wnd: 32, ..KcpConfig::fast_mode() });
    // 只有 frg=255 的头分片，后续永远不来
    let pkt = raw_packet(RawHeader { frg: 255, ..RawHeader::push(1, 0) }, b"head");
    sink.kcp.borrow_mut().input(&pkt).unwrap();

    for _ in 0..200 {
        sink.update(100).await.unwrap();
    }
    assert!(sink.kcp.borrow().nrcv_que <= 1, "残缺 frg 链不应堆积");
    let err = sink.kcp.borrow_mut().peeksize().unwrap_err();
    assert_eq!(err.kind(), KcpErrorKind::IncompleteData);
}

/// 大量重复的同一段：只交付一次，且缓存有界
#[tokio::test]
async fn massive_duplicate_burst() {
    let sink = Sink::new(KcpConfig::stream_mode());
    let pkt = push_packet(1, 0, b"payload");
    for _ in 0..20_000 {
        sink.kcp.borrow_mut().input(&pkt).unwrap();
    }
    let got = sink.kcp.borrow_mut().recv().unwrap();
    assert_eq!(&got[..], b"payload");
    assert_eq!(sink.kcp.borrow().nrcv_buf, 0);
    assert_eq!(sink.kcp.borrow().nrcv_que, 0);
}

/// conv 不匹配的包必须整体忽略，不改变任何状态
#[tokio::test]
async fn foreign_conv_does_not_touch_state() {
    let sink = Sink::new(KcpConfig::stream_mode());
    sink.kcp.borrow_mut().input(&push_packet(1, 0, b"good")).unwrap();
    let before = (
        sink.kcp.borrow().rcv_nxt,
        sink.kcp.borrow().nrcv_buf,
        sink.kcp.borrow().rmt_wnd,
        sink.kcp.borrow().peer_seen,
    );

    for _ in 0..1000 {
        sink.kcp.borrow_mut().input(&push_packet(0xDEAD, 1, b"evil")).unwrap();
        sink.kcp.borrow_mut().input(&control_packet(0xDEAD, KcpCmd::Wask, 0)).unwrap();
    }
    let after = (
        sink.kcp.borrow().rcv_nxt,
        sink.kcp.borrow().nrcv_buf,
        sink.kcp.borrow().rmt_wnd,
        sink.kcp.borrow().peer_seen,
    );
    assert_eq!(before, after, "外来 conv 的包改变了本地状态");
}

/// 载荷内容为全 0 / 全 0xFF / 高位字节时都必须原样交付
#[tokio::test]
async fn payload_bit_patterns_preserved() {
    let sink = Sink::new(KcpConfig::stream_mode());
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0u8; 256]);
    payload.extend_from_slice(&[0xFFu8; 256]);
    payload.extend((0..=255u8).collect::<Vec<u8>>());

    for (sn, chunk) in (0u32..).zip(payload.chunks(100)) {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, chunk)).unwrap();
    }
    let got = sink.kcp.borrow_mut().recv().unwrap();
    assert_eq!(checksum(&got), checksum(&payload));
    assert_eq!(&got[..], &payload[..]);
}

/// 极端窗口值（0 和 u16::MAX）不应引发除零或溢出
#[tokio::test]
async fn extreme_window_values_safe() {
    for wnd in [0u16, 1, u16::MAX] {
        let mut sink = Sink::new(KcpConfig::stream_mode());
        sink.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, wnd)).unwrap();
        sink.kcp.borrow_mut().input(&push_packet(1, 0, b"x")).unwrap();
        sink.update(100).await.unwrap();
        sink.kcp.borrow_mut().send(b"reply").unwrap();
        sink.update(120).await.unwrap();
        // 不 panic 即为通过；同时 snd_buf 不应无界
        assert!(sink.kcp.borrow().snd_buf.len() <= 1, "wnd={wnd} 时 snd_buf 异常增长");
    }
}

/// 超长载荷（超过 MTU）应被接受解析，而不是越界读
#[test]
fn oversized_payload_parsed_safely() {
    let sink = Sink::new(KcpConfig { mtu: 128, ..KcpConfig::default() });
    let big = vec![0xABu8; 8000];
    let pkt = push_packet(1, 0, &big);
    sink.kcp.borrow_mut().input(&pkt).unwrap();
    let got = sink.kcp.borrow_mut().recv().unwrap();
    assert_eq!(got.len(), 8000);
}
