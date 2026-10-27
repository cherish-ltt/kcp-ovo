//! 黑盒测试：窗口、流控与边界行为
//!
//! 验证窗口通告、窗口探测、超大消息拒绝、跨 conv 隔离等真实部署中会踩到的边界。

mod common;

use common::{Net, Rng, Sim, Solo, assert_same, parse_cmd};
use kcp_ovo::{KcpCmd, KcpConfig};

/// 接收窗口很小时，发送端不应突破对端通告的窗口
#[tokio::test]
async fn sender_respects_small_remote_window() {
    let cfg_a = KcpConfig { stream_mode: true, ..KcpConfig::fast_mode_with_wnd(512, 512) };
    let cfg_b = KcpConfig { rcv_wnd: 8, ..cfg_a.clone() };
    let mut sim = Sim::asymmetric(cfg_a, cfg_b, Net::default(), 41);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(41).bytes(mss * 40);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    for _ in 0..40000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 两端窗口不对称时，大窗口侧要等小窗口侧通告后才能放量，但数据必须完整
#[tokio::test]
async fn asymmetric_window_still_delivers() {
    let cfg_a = KcpConfig { stream_mode: true, ..KcpConfig::fast_mode_with_wnd(1024, 1024) };
    let cfg_b = KcpConfig { rcv_wnd: 32, ..cfg_a.clone() };
    let mut sim = Sim::asymmetric(cfg_a, cfg_b, Net::default(), 42);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(42).bytes(mss * 200);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    for _ in 0..80000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 非流模式下超过 255 分片的消息必须被显式拒绝，而不是静默截断
#[test]
fn oversized_message_rejected() {
    let cfg = KcpConfig::fast_mode();
    let probe = kcp_ovo::core::kcp::Kcp::new(1, cfg).unwrap();
    let mss = probe.mss as usize;
    // 255 段允许，256 段拒绝
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::fast_mode()).unwrap();
    assert!(kcp.send(&vec![0u8; mss * 255]).is_ok());
    assert!(kcp.send(&vec![0u8; mss * 256]).is_err());
}

/// 流模式下同样的数据量不应报错
#[test]
fn stream_mode_accepts_large_message() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    let mss = kcp.mss as usize;
    assert!(kcp.send(&vec![0u8; mss * 600]).is_ok());
    assert_eq!(kcp.nsnd_que, 600);
}

/// 未被任何包证实过的发送端，必须仍能发出第一个探路段
#[tokio::test]
async fn first_segment_probe_path() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 43);
    // 两端刚创建，谁都没收到过对端包
    assert_eq!(sim.a.borrow().rmt_wnd, 0);
    sim.a.borrow_mut().send(b"bootstrap").unwrap();
    sim.tick().await.unwrap();
    assert_eq!(sim.a.borrow().nsnd_buf, 1, "对端窗口未知时应放行 1 段探路");
}

/// 对端窗口长期为 0 时，应发出窗口探测 Wask 而不是静默空转
#[tokio::test]
async fn window_probe_is_emitted_when_peer_window_closed() {
    let mut solo = Solo::new(KcpConfig::stream_mode());
    solo.kcp.borrow_mut().input(&window_update(0)).ok();
    solo.kcp.borrow_mut().send(b"data").unwrap();

    let mut saw_wask = false;
    for _ in 0..4000 {
        solo.step(20).await.unwrap();
        for pkt in solo.take_out() {
            if parse_cmd(&pkt) == Some(KcpCmd::Wask) {
                saw_wask = true;
                break;
            }
        }
        if saw_wask {
            break;
        }
    }
    assert!(saw_wask, "对端窗口为 0 时必须发窗口探测，否则发送端会永久卡死");
}

/// 收到 Wask 的对端必须回 Wins，使探测方重新获知窗口
#[tokio::test]
async fn wask_triggers_wins_reply() {
    let mut solo = Solo::new(KcpConfig::stream_mode());
    let conv = solo.kcp.borrow().conv;
    solo.kcp.borrow_mut().input(&wask_packet(conv)).ok();
    let mut saw_wins = false;
    for _ in 0..50 {
        solo.step(20).await.unwrap();
        for pkt in solo.take_out() {
            if parse_cmd(&pkt) == Some(KcpCmd::Wins) {
                saw_wins = true;
                break;
            }
        }
        if saw_wins {
            break;
        }
    }
    assert!(saw_wins, "收到 Wask 后必须回 Wins");
}

/// conv 不匹配的包必须被静默丢弃，不得污染接收队列
#[tokio::test]
async fn foreign_conv_packets_ignored() {
    let cfg = KcpConfig::stream_mode();
    let mut sim = Sim::with_conv(0x1111, 0x1111, cfg.clone(), cfg, Net::default(), 44);
    sim.a.borrow_mut().send(b"payload").unwrap();
    let good = sim.drain_a_to_b(400).await.unwrap();
    assert_same(&good, b"payload");

    // 注入一个不同 conv 的数据包
    let foreign = make_data_packet(0x2222, 0xAAAA_BBBB, b"evil");
    let err = sim.b.borrow_mut().input(&foreign);
    assert!(err.is_ok(), "异 conv 包应被静默丢弃而不是报错");
    assert!(sim.take_b().is_empty(), "异 conv 包不得进入接收队列");
}

/// 接收端 conv 为 0 时应从首个包学习 conv，然后只接受同 conv
#[tokio::test]
async fn server_learns_conv_from_first_packet() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(0, KcpConfig::stream_mode()).unwrap();
    let pkt = make_data_packet(0x5566_7788, 0, b"first");
    kcp.input(&pkt).unwrap();
    assert_eq!(kcp.conv, 0x5566_7788, "conv=0 的服务端应学习首个包的 conv");

    let other = make_data_packet(0x0000_1111, 1, b"second");
    kcp.input(&other).unwrap();
    let got = kcp.recv().unwrap();
    assert_same(&got, b"first");
    // recv 后不应还有 second
    assert!(kcp.recv().is_err());
}

/// 损坏/截断的输入包返回错误而不是 panic
#[test]
fn truncated_input_returns_error() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    assert!(kcp.input(&[]).is_err());
    assert!(kcp.input(&[0u8; 10]).is_err());
    assert!(kcp.input(&[0u8; 27]).is_err());
}

/// 非法命令字返回错误而不是被当成数据
#[test]
fn unknown_command_rejected() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    let mut raw = vec![0u8; 28];
    raw[0..4].copy_from_slice(&1u32.to_be_bytes());
    raw[4] = 99; // 非法 cmd
    assert!(kcp.input(&raw).is_err());
}

/// 负载长度字段大于实际数据时返回错误
#[test]
fn payload_shorter_than_declared_rejected() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    let mut raw = vec![0u8; 28 + 4];
    raw[0..4].copy_from_slice(&1u32.to_be_bytes());
    raw[4] = 81;
    raw[24..28].copy_from_slice(&100u32.to_be_bytes()); // 声明 100 字节，实际只有 4
    assert!(kcp.input(&raw).is_err());
}

/// 窗口为 0 的接收端应暂停向外交付新数据（rcv_queue 满）
#[tokio::test]
async fn zero_receive_window_blocks_queue_growth() {
    let cfg = KcpConfig { rcv_wnd: 1, ..KcpConfig::stream_mode() };
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, cfg).unwrap();
    // 依次投入 3 段，rcv_wnd=1 时 rcv_queue 最多容纳 1 段
    for sn in 0..3u32 {
        let pkt = make_data_packet(1, sn, b"xyz");
        kcp.input(&pkt).unwrap();
    }
    assert!(kcp.nrcv_que <= 1, "rcv_queue 不得超出接收窗口");
}

fn make_data_packet(conv: u32, sn: u32, payload: &[u8]) -> Vec<u8> {
    let mut raw = vec![0u8; 28 + payload.len()];
    raw[0..4].copy_from_slice(&conv.to_be_bytes());
    raw[4] = KcpCmd::Push as u8;
    raw[5] = 0;
    raw[6..8].copy_from_slice(&1024u16.to_be_bytes());
    raw[8..16].copy_from_slice(&0u64.to_be_bytes());
    raw[16..20].copy_from_slice(&sn.to_be_bytes());
    raw[20..24].copy_from_slice(&0u32.to_be_bytes());
    raw[24..28].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    raw[28..].copy_from_slice(payload);
    raw
}

fn wask_packet(conv: u32) -> Vec<u8> {
    let mut raw = vec![0u8; 28];
    raw[0..4].copy_from_slice(&conv.to_be_bytes());
    raw[4] = KcpCmd::Wask as u8;
    raw[6..8].copy_from_slice(&1024u16.to_be_bytes());
    raw
}

fn window_update(wnd: u16) -> Vec<u8> {
    let mut raw = vec![0u8; 28];
    raw[0..4].copy_from_slice(&0u32.to_be_bytes());
    raw[4] = KcpCmd::Wins as u8;
    raw[6..8].copy_from_slice(&wnd.to_be_bytes());
    raw
}

/// 已知约束：非流模式下，单条消息的分片数超过接收窗口会导致无法 assemble
///
/// `peeksize()` 需要等整条消息的 frg 链凑齐，但接收窗口限制了 rcv_queue 的容量，
/// 窗口外的分片进不来 → 永远凑不齐 → recv 永久返回 PayloadWaitForFrg。
/// 该测试固定这一行为，避免将来误以为是偶发问题；使用方需保证
/// 单条消息分片数 <= rcv_wnd，或改用 stream_mode。
#[tokio::test]
async fn message_mode_larger_than_rcv_window_stalls() {
    let cfg = KcpConfig { rcv_wnd: 8, ..KcpConfig::fast_mode() };
    let mut sim = Sim::new(cfg, Net::default(), 45);
    let mss = sim.a.borrow().mss as usize;
    // 20 段 = 一条消息，超过接收窗口 8
    let msg = Rng::new(45).bytes(mss * 20);
    sim.a.borrow_mut().send(&msg).unwrap();

    sim.run(3000).await.unwrap();
    let got = sim.b_messages();
    assert!(got.is_empty(), "分片数超过接收窗口时整条消息无法交付");
    assert!(sim.b.borrow().nrcv_que > 0, "窗口内已收到的分片仍滞留在队列");
}

/// 同样数据量改用流模式即可正常交付，证明问题只在消息组装语义
#[tokio::test]
async fn stream_mode_same_data_does_not_stall() {
    let cfg = KcpConfig { rcv_wnd: 8, stream_mode: true, ..KcpConfig::fast_mode() };
    let mut sim = Sim::new(cfg, Net::default(), 46);
    let mss = sim.a.borrow().mss as usize;
    let msg = Rng::new(46).bytes(mss * 20);
    sim.a.borrow_mut().send(&msg).unwrap();

    let mut got = Vec::new();
    for _ in 0..20000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= msg.len() {
            break;
        }
    }
    assert_same(&got, &msg);
}
