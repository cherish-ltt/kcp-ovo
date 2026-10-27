//! 黑盒测试：敌意输入、配置边界与窗口账本
//!
//! 这一组用例专门攻击「看起来不会发生」的地方：对端发来的序号/长度字段可能是
//! 任意值，配置可能是任何 `u32`/`u16`，链路可能把包吞掉。断言只依赖公开 API
//! 与文档化字段，全部使用虚拟时钟与固定种子。

mod common;

use common::stress::{RawHeader, Sink, control_packet, push_packet, raw_packet};
use common::{Net, Rng, Sim, Solo, assert_same};
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::{KcpCmd, KcpConfig, KcpError};

/// 对端上报的 `una` 越过 `snd_nxt` 时，不得把尚未发送成功的段当成已确认
///
/// 链路上的比特翻转足以把 una 抬到任意值。若照单全收，发送方会清空 `snd_buf`
/// 并把 `snd_una` 推到 snd_nxt 之外：之后所有真实 ACK 都被判成过期，
/// 数据既不在队列里、也永远不会重传，表现为静默丢数据。
#[tokio::test]
async fn una_ahead_of_snd_nxt_must_not_discard_unacked_segments() {
    let mut solo = Solo::new(KcpConfig::stream_mode());
    let conv = solo.kcp.borrow().conv;
    solo.kcp.borrow_mut().send(&vec![0x5Au8; 4000]).unwrap();
    solo.step(0).await.unwrap();

    let (inflight, snd_nxt) = {
        let k = solo.kcp.borrow();
        (k.nsnd_buf, k.snd_nxt)
    };
    assert!(inflight > 0, "首轮 flush 后应有在途段");
    assert_eq!(solo.kcp.borrow().snd_una, 0);

    solo.kcp.borrow_mut().input(&forged_ack(conv, 0, snd_nxt + 1000)).unwrap();

    let k = solo.kcp.borrow();
    assert_eq!(k.nsnd_buf, inflight, "una 超前时不得清空在途段");
    assert_eq!(k.snd_una, 0, "snd_una 不得越过 snd_nxt");
    assert!(k.waitsnd() > 0, "数据被误判为已确认，已构成静默丢数据");
}

/// 拒收超前 una 之后，合法的 una 仍必须推进确认
#[tokio::test]
async fn valid_una_still_acknowledges_after_forward_una_rejected() {
    let mut solo = Solo::new(KcpConfig::stream_mode());
    let conv = solo.kcp.borrow().conv;
    solo.kcp.borrow_mut().send(&vec![0x11u8; 4000]).unwrap();
    solo.step(0).await.unwrap();
    let snd_nxt = solo.kcp.borrow().snd_nxt;
    assert!(snd_nxt > 0, "首轮 flush 后应有已发送段");

    solo.kcp.borrow_mut().input(&forged_ack(conv, 0, snd_nxt + 1000)).unwrap();
    assert_eq!(solo.kcp.borrow().snd_una, 0, "超前 una 不得生效");

    // 真实确认（恰好确认全部已发送段）必须照常生效
    solo.kcp.borrow_mut().input(&forged_ack(conv, 0, snd_nxt)).unwrap();
    let k = solo.kcp.borrow();
    assert_eq!(k.nsnd_buf, 0, "合法 una 必须清空已确认段");
    assert_eq!(k.snd_una, snd_nxt, "合法 una 必须推进 snd_una");
}

/// 在途段数必须受对端接收窗口约束，不能随着 flush 次数无界增长
///
/// 发送窗口约束的是「已发出但未确认」的段数。若每轮 flush 只按窗口大小放行新段、
/// 却不扣除已在途的额度，慢链路上每轮都会再塞满一个窗口，实际在途量远超对端容量：
/// 接收端只能丢弃，重传风暴随之而来，`snd_buf` 也会无界增长。
#[tokio::test]
async fn in_flight_segments_never_exceed_peer_receive_window() {
    let cfg_a = KcpConfig { snd_wnd: 64, rcv_wnd: 64, ..KcpConfig::stream_mode() };
    let cfg_b = KcpConfig { rcv_wnd: 8, ..cfg_a.clone() };
    let mut sim = Sim::asymmetric(cfg_a, cfg_b, Net::default().with_delay(50), 0xAD01);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(0xAD01).bytes(mss * 100);
    sim.a.borrow_mut().send(&payload).unwrap();

    let peer_capacity = sim.b.borrow().rcv_wnd as usize;
    let mut got = Vec::new();
    for _ in 0..20_000 {
        sim.tick().await.unwrap();
        let inflight = sim.a.borrow().snd_buf.len();
        assert!(
            inflight <= peer_capacity,
            "在途 {} 段超过对端接收窗口 {peer_capacity} 段",
            inflight
        );
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 对端一声不吭时，只允许放行 1 段探路，不能每轮再塞 1 段
#[tokio::test]
async fn unknown_peer_admits_only_one_probe_segment() {
    let mut solo = Solo::new(KcpConfig { snd_wnd: 64, ..KcpConfig::stream_mode() });
    solo.kcp.borrow_mut().send(&vec![0x77u8; 4000]).unwrap();

    for _ in 0..20 {
        solo.step(20).await.unwrap();
    }
    assert_eq!(
        solo.kcp.borrow().nsnd_buf,
        1,
        "对端从未响应时在途段涨到了 {}，探路额度没有按段计",
        solo.kcp.borrow().nsnd_buf
    );
}

/// 退化配置必须被构造期拒绝，而不是在运行期 panic 或永久不可用
#[test]
fn degenerate_configs_are_rejected() {
    for mtu in [0u32, 1, 27, 28] {
        let result = Kcp::new(1, KcpConfig { mtu, ..KcpConfig::default() });
        assert!(
            matches!(result, Err(KcpError::InvalidConfig(_))),
            "mtu={mtu} 应被拒绝，实际 {:?}",
            result.map(|k| k.mss)
        );
    }
    for (snd_wnd, rcv_wnd) in [(0u16, 64u16), (64, 0)] {
        let result = Kcp::new(1, KcpConfig { snd_wnd, rcv_wnd, ..KcpConfig::default() });
        assert!(
            matches!(result, Err(KcpError::InvalidConfig(_))),
            "snd_wnd={snd_wnd} rcv_wnd={rcv_wnd} 应被拒绝"
        );
    }
}

/// 允许的最小配置必须真的能用：mtu 刚好能装下 1 字节载荷
#[tokio::test]
async fn minimum_usable_config_transfers() {
    let cfg = KcpConfig { mtu: 29, snd_wnd: 4, rcv_wnd: 4, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, Net::default(), 0xAD02);
    assert_eq!(sim.a.borrow().mss, 1);
    let payload = b"hello, minimal mtu".to_vec();
    sim.a.borrow_mut().send(&payload).unwrap();
    let mut got = Vec::new();
    for _ in 0..20_000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert_same(&got, &payload);
}

/// 同轮同时有 ACK 与到期探测时，两者都要发出；`check()` 不得要求立刻 update
///
/// 探测若被 ACK 挤掉，`probe & IKCP_ASK_SEND` 会一直挂着，`check()` 于是恒返回 current。
/// 调用方按此立即 update，又被 `ts_flush` 节流掉，形成纯空转 —— 零窗口期间 CPU 打满
/// 却一个包也发不出去。
#[tokio::test]
async fn probe_is_not_swallowed_by_pending_ack() {
    let mut sink = Sink::new(KcpConfig::fast_mode());
    // 先制造一个待回 ACK 的数据段，再把对端窗口压到 0，使探测与 ACK 同轮到齐
    sink.kcp.borrow_mut().input(&push_packet(1, 0, b"data")).unwrap();
    sink.kcp.borrow_mut().input(&control_packet(1, KcpCmd::Wins, 0)).unwrap();

    sink.clear();
    sink.update(20).await.unwrap();
    assert_eq!(sink.count_cmd(KcpCmd::Ack), 1, "数据段的 ACK 必须发出");
    assert_eq!(sink.count_cmd(KcpCmd::Wask), 1, "零窗口探测不得被同轮 ACK 吞掉");

    let next = sink.kcp.borrow().check(20);
    assert!(next > 20, "check 返回 {next}，要求立刻 update，调用方会全速空转");
    assert_eq!(next, 20 + kcp_ovo::helper::IKCP_PROBE_INIT);
}

/// 头部字段全取极值时，解析路径既不能 panic，也不能污染队列账本
#[tokio::test]
async fn hostile_header_sweep_keeps_state_consistent() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("una 超前", forged_ack(1, 0, u32::MAX)),
        ("sn 回绕边界", push_packet(1, u32::MAX, b"wrap")),
        ("sn 窗口边缘", push_packet(1, 7, b"edge")),
        ("frg 满值", raw_packet(RawHeader { frg: 255, ..RawHeader::push(1, 0) }, b"head")),
        ("wnd 满值", raw_packet(RawHeader { wnd: u16::MAX, ..RawHeader::push(1, 0) }, b"w")),
        ("ts 满值", raw_packet(RawHeader { ts: u64::MAX, ..RawHeader::push(1, 0) }, b"t")),
        ("len 超大", declared_len_packet(1, 0, u32::MAX, b"short")),
        ("WASK 洪水", control_packet(1, KcpCmd::Wask, 0)),
        ("WINS 零窗", control_packet(1, KcpCmd::Wins, 0)),
    ];

    for (name, packet) in cases {
        let mut sink = Sink::new(KcpConfig { rcv_wnd: 8, ..KcpConfig::stream_mode() });
        for _ in 0..50 {
            let _ = sink.kcp.borrow_mut().input(&packet);
        }
        sink.update(50).await.unwrap();
        {
            let k = sink.kcp.borrow();
            assert!(k.nrcv_buf <= k.rcv_wnd as usize, "{name}: rcv_buf 越界");
            assert!(k.nrcv_que <= k.rcv_wnd as usize, "{name}: rcv_queue 越界");
            assert!(k.snd_buf.len() <= k.snd_wnd as usize, "{name}: snd_buf 越界");
            assert!(k.mss > 0, "{name}: mss 被破坏");
        }

        // 敌意输入之后仍必须能完成一次正常发送
        sink.kcp.borrow_mut().send(b"alive").unwrap();
        sink.update(100).await.unwrap();
        assert!(!sink.emitted().is_empty(), "{name}: 异常输入后无法发送");
    }
}

/// 进行中的连接被超前 una 洪水攻击后，仍须把数据完整送达并完成对账
#[tokio::test]
async fn forward_una_flood_does_not_kill_live_connection() {
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 64, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, Net::loss(0.1).with_delay(6).with_jitter(3), 0xAD03);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(0xAD03).bytes(mss * 60);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    for round in 0..4_000 {
        sim.tick().await.unwrap();
        if round % 7 == 0 {
            let (conv, snd_nxt) = {
                let a = sim.a.borrow();
                (a.conv, a.snd_nxt)
            };
            for _ in 0..10 {
                let _ = sim.a.borrow_mut().input(&forged_ack(conv, 0, snd_nxt + 5000));
            }
        }
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert_same(&got, &payload);
    assert_eq!(sim.a.borrow().waitsnd(), 0, "攻击结束后发送方仍未完成对账");
    assert!(sim.a.borrow().snd_una <= sim.a.borrow().snd_nxt, "snd_una 越过 snd_nxt");
}

/// 构造一个携带任意 una 的 ACK 包
fn forged_ack(conv: u32, sn: u32, una: u32) -> Vec<u8> {
    raw_packet(
        RawHeader { conv, cmd: KcpCmd::Ack as u8, wnd: 1024, sn, una, ..RawHeader::default() },
        b"",
    )
}

/// 构造一个头部声明长度与真实载荷不符的 Push 包
fn declared_len_packet(conv: u32, sn: u32, declared: u32, payload: &[u8]) -> Vec<u8> {
    let mut packet = raw_packet(RawHeader::push(conv, sn), payload);
    packet[24..28].copy_from_slice(&declared.to_be_bytes());
    packet
}
