//! 黑盒测试：公开类型的行为契约
//!
//! 覆盖错误类型的分类/展示、命令字编解码、数据包编解码边界、
//! 以及队列容器的公开语义。这些都是使用者会直接接触的 API 表面。

mod common;

use bytes::Bytes;

use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::error::{IncompleteDataType, KcpErrorKind};
use kcp_ovo::helper::{KcpPacket, KcpPacketHeader};
use kcp_ovo::queue::{KcpDeque, Segment};
use kcp_ovo::{KcpCmd, KcpConfig, KcpError};

// ===== 错误类型 =====

/// 每个错误变体都能映射到独立的分类
#[test]
fn every_error_has_distinct_kind() {
    let cases = [
        (KcpError::InvalidCommand(99), KcpErrorKind::InvalidCommand),
        (KcpError::BufferTooSmall, KcpErrorKind::BufferTooSmall),
        (KcpError::QueueEmpty, KcpErrorKind::QueueEmpty),
        (KcpError::IncompleteData(IncompleteDataType::Header), KcpErrorKind::IncompleteData),
        (KcpError::InvalidSequence, KcpErrorKind::InvalidSequence),
        (KcpError::InvalidConfig("x".into()), KcpErrorKind::InvalidConfig),
        (KcpError::OutputNotSet, KcpErrorKind::OutputNotSet),
        (KcpError::IoError("x".into()), KcpErrorKind::IoError),
        (KcpError::NoAddress, KcpErrorKind::NoAddress),
    ];
    for (err, expected) in cases {
        assert_eq!(err.kind(), expected, "错误 {err:?} 的分类不匹配");
    }
}

/// 所有错误都必须能 Display，且非空
#[test]
fn every_error_is_displayable() {
    let errors = [
        KcpError::InvalidCommand(7),
        KcpError::BufferTooSmall,
        KcpError::QueueEmpty,
        KcpError::IncompleteData(IncompleteDataType::Payload),
        KcpError::InvalidSequence,
        KcpError::InvalidConfig("bad".into()),
        KcpError::OutputNotSet,
        KcpError::IoError("io".into()),
        KcpError::NoAddress,
    ];
    for err in errors {
        let text = err.to_string();
        assert!(!text.is_empty(), "错误 {err:?} 的 Display 为空");
    }
}

/// 错误实现了 std::error::Error，可接入常规错误处理链
#[test]
fn error_implements_std_error() {
    fn assert_error<E: std::error::Error>(_: &E) {}
    assert_error(&KcpError::QueueEmpty);
}

/// io::Error 可以转换成 KcpError
#[test]
fn io_error_converts() {
    let io = std::io::Error::other("boom");
    let err: KcpError = io.into();
    assert_eq!(err.kind(), KcpErrorKind::IoError);
}

/// IncompleteDataType 的四种取值都有可读描述
#[test]
fn incomplete_data_type_display() {
    for t in [
        IncompleteDataType::Header,
        IncompleteDataType::Payload,
        IncompleteDataType::PayloadWaitForFrg,
        IncompleteDataType::PayloadErr,
    ] {
        assert!(!t.to_string().is_empty());
    }
}

// ===== 命令字 =====

/// 命令字与字节值双向一致
#[test]
fn command_roundtrip() {
    for cmd in [KcpCmd::Push, KcpCmd::Ack, KcpCmd::Wask, KcpCmd::Wins] {
        let b: u8 = cmd.into();
        assert_eq!(KcpCmd::try_from(b).unwrap(), cmd);
        let n: u32 = cmd.into();
        assert_eq!(n, b as u32);
    }
}

/// 四个命令字的字节值符合协议约定
#[test]
fn command_wire_values() {
    assert_eq!(u8::from(KcpCmd::Push), 81);
    assert_eq!(u8::from(KcpCmd::Ack), 82);
    assert_eq!(u8::from(KcpCmd::Wask), 83);
    assert_eq!(u8::from(KcpCmd::Wins), 84);
}

/// 非法命令字必须被拒绝
#[test]
fn invalid_command_rejected() {
    for bad in [0u8, 1, 80, 85, 255] {
        assert_eq!(KcpCmd::try_from(bad).unwrap_err(), KcpError::InvalidCommand(bad as u32));
    }
}

// ===== 数据包编解码 =====

/// 头部编码后能原样解析回来
#[test]
fn header_roundtrip() {
    let header =
        KcpPacketHeader::new(0xDEAD_BEEF, KcpCmd::Ack, 3, 256, 1_700_000_000_000, 42, 40, 128);
    let encoded = header.to_bytes().unwrap();
    assert_eq!(encoded.len(), 28);
    let mut buf = Bytes::from(encoded.to_vec());
    let (parsed, rest) = KcpPacketHeader::from_bytes(&mut buf).unwrap();
    assert_eq!(parsed, header);
    assert!(rest.is_empty());
}

/// ts 字段是 u64，超过 u32 范围的值必须保留
#[test]
fn header_timestamp_beyond_u32() {
    let big = u32::MAX as u64 + 12345;
    let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 1, big, 0, 0, 0);
    let encoded = header.to_bytes().unwrap();
    let mut buf = Bytes::from(encoded.to_vec());
    let (parsed, _) = KcpPacketHeader::from_bytes(&mut buf).unwrap();
    assert_eq!(parsed.ts, big, "时间戳必须按 u64 保留");
}

/// 完整包解析应同时带回头部与负载
#[test]
fn packet_roundtrip_with_payload() {
    let payload = b"hello-kcp";
    let header = KcpPacketHeader::new(7, KcpCmd::Push, 0, 512, 1000, 9, 8, payload.len() as u32);
    let packet = KcpPacket::new_with_data(header, Bytes::copy_from_slice(payload));
    let encoded = packet.to_bytes().unwrap();
    let mut buf = Bytes::from(encoded.to_vec());
    let parsed = KcpPacket::from_bytes(&mut buf).unwrap();
    assert_eq!(parsed.header, header);
    assert_eq!(parsed.data.as_ref(), payload);
}

/// 声明长度大于实际负载时必须报错
#[test]
fn packet_short_payload_errors() {
    let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 1, 0, 0, 0, 100);
    let err = KcpPacket::from_header_and_data(header, &[1, 2, 3]).unwrap_err();
    assert_eq!(err, KcpError::IncompleteData(IncompleteDataType::Payload));
}

/// 不足一个头部长度就解析必须报错
#[test]
fn packet_truncated_header_errors() {
    for len in [0usize, 1, 27] {
        let mut buf = Bytes::from(vec![0u8; len]);
        let err = KcpPacketHeader::from_bytes(&mut buf).unwrap_err();
        assert_eq!(err, KcpError::IncompleteData(IncompleteDataType::Header));
    }
}

// ===== 队列容器 =====

/// KcpDeque 保持 FIFO 语义
#[test]
fn deque_fifo_order() {
    let mut q = KcpDeque::new();
    assert!(q.is_empty());
    for i in 0..5u32 {
        q.push_back(make_seg(i));
    }
    assert_eq!(q.len(), 5);
    for i in 0..5u32 {
        assert_eq!(q.pop_front().unwrap().kcp_packet.header.sn, i);
    }
    assert!(q.is_empty());
    assert!(q.pop_front().is_none());
}

/// push_front / pop_back 组合符合双端队列语义
#[test]
fn deque_both_ends() {
    let mut q = KcpDeque::new();
    q.push_back(make_seg(1));
    q.push_front(make_seg(0));
    assert_eq!(q.front().unwrap().kcp_packet.header.sn, 0);
    assert_eq!(q.back().unwrap().kcp_packet.header.sn, 1);
    assert_eq!(q.pop_back().unwrap().kcp_packet.header.sn, 1);
    assert_eq!(q.pop_front().unwrap().kcp_packet.header.sn, 0);
}

/// 队列上的可变访问可以就地修改元素
#[test]
fn deque_mutation_via_front_mut() {
    let mut q = KcpDeque::new();
    q.push_back(make_seg(0));
    q.front_mut().unwrap().xmit = 9;
    q.back_mut().unwrap().fastack = 4;
    let seg = q.front().unwrap();
    assert_eq!(seg.xmit, 9);
    assert_eq!(seg.fastack, 4);
}

/// get_mut 能按索引取到可变引用
#[test]
fn deque_get_mut() {
    let mut q = KcpDeque::new();
    for i in 0..3u32 {
        q.push_back(make_seg(i));
    }
    q.get_mut(1).unwrap().rto = 777;
    assert_eq!(q.get_mut(1).unwrap().rto, 777);
}

/// Segment 的字段默认值与大小计算
#[test]
fn segment_defaults_and_size() {
    let seg = make_seg(5);
    assert_eq!(seg.xmit, 0);
    assert_eq!(seg.fastack, 0);
    assert_eq!(seg.rto, 0);
    assert_eq!(seg.resendts, 0);
    let len = seg.kcp_packet.data.len();
    assert_eq!(seg.size(), 28 + len);
    assert_eq!(seg.get_conv(), 1);
    assert_eq!(seg.get_cmd(), KcpCmd::Push);
}

fn make_seg(sn: u32) -> Segment {
    let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 0, sn, 0, 4);
    Segment::new_with_header_and_data(header, b"data").unwrap()
}

// ===== Kcp 公开状态 =====

/// 流模式与非流模式下 mss 计算一致
#[test]
fn mss_independent_of_mode() {
    let a = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    let b = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
    assert_eq!(a.mss, b.mss);
    assert_eq!(a.mss, a.mtu - 28);
}

/// 自定义 mtu 会同步影响 mss
#[test]
fn custom_mtu_reflected_in_mss() {
    let kcp = Kcp::new(1, KcpConfig { mtu: 1200, ..KcpConfig::default() }).unwrap();
    assert_eq!(kcp.mtu, 1200);
    assert_eq!(kcp.mss, 1172);
}

/// normal_mode 应等价于 Default
#[test]
fn normal_mode_equals_default() {
    let m = KcpConfig::normal_mode();
    let d = KcpConfig::default();
    assert_eq!(m.mtu, d.mtu);
    assert_eq!(m.interval, d.interval);
    assert_eq!(m.nodelay, d.nodelay);
    assert_eq!(m.fastresend, d.fastresend);
    assert_eq!(m.nocwnd, d.nocwnd);
    assert_eq!(m.snd_wnd, d.snd_wnd);
    assert_eq!(m.rcv_wnd, d.rcv_wnd);
    assert_eq!(m.stream_mode, d.stream_mode);
}

/// iter_mut 允许就地修改队列内所有元素
#[test]
fn deque_iter_mut_visits_all() {
    let mut q = KcpDeque::new();
    for i in 0..4u32 {
        q.push_back(make_seg(i));
    }
    for seg in q.iter_mut() {
        seg.rto = 1234;
    }
    assert!(q.iter().all(|s| s.rto == 1234));
    assert_eq!(q.iter().count(), 4);
}

/// 窗口配置应被如实反映到实例状态
#[test]
fn window_config_applied() {
    let kcp = Kcp::new(1, KcpConfig::fast_mode_with_wnd(256, 512)).unwrap();
    assert_eq!(kcp.snd_wnd, 256);
    assert_eq!(kcp.rcv_wnd, 512);
}
