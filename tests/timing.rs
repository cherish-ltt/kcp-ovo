//! 黑盒测试：ACK 语义、快速重传与收发时序
//!
//! ACK 是 KCP 可靠性的枢纽。丢一个 ACK 与丢一个数据段对发送方是同一件事，
//! 因此接收端必须对任何重传都给出确认。这里把这些时序场景固化成回归用例。

mod common;

use common::stress::{RawHeader, Sink, push_packet};
use common::{Net, Rng, Sim, assert_same};
use kcp_ovo::KcpConfig;
use kcp_ovo::helper::KcpCmd;

/// ACK 丢失后，发送方重传同一段，接收方必须补回 ACK
///
/// 这是最容易被忽略的一条：接收方已经在 rcv_queue 里有了该段，
/// 若简单地按「重复」丢弃，双方就永远对不上账。
#[tokio::test]
async fn duplicate_segment_triggers_ack() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    let pkt = push_packet(1, 0, b"hello");

    sink.kcp.borrow_mut().input(&pkt).unwrap();
    assert_eq!(sink.kcp.borrow_mut().recv().unwrap().len(), 5);
    sink.update(20).await.unwrap();
    let first_acks = sink.count_cmd(KcpCmd::Ack);
    assert_eq!(first_acks, 1, "首次收到数据段应回 1 个 ACK");

    // ACK 在网络上丢了，发送方超时重传同一段
    sink.clear();
    sink.kcp.borrow_mut().input(&pkt).unwrap();
    sink.update(40).await.unwrap();
    assert_eq!(sink.count_cmd(KcpCmd::Ack), 1, "重传段必须补回 ACK");
    assert!(sink.kcp.borrow_mut().recv().is_err(), "重复段不应二次交付");
}

/// 端到端：随机丢包下 ACK 丢失不会让连接卡死
#[tokio::test]
async fn ack_loss_does_not_stall_connection() {
    for seed in 0..6u64 {
        let mut rng = Rng::new(0xA0C0_0000 + seed);
        let net = Net::loss(0.5).with_delay(8);
        let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(8, 32), net, 0xA0C0_1000 + seed);
        let payload = rng.bytes(sim.a.borrow().mss as usize * 6);
        sim.a.borrow_mut().send(&payload).unwrap();

        let mut got = Vec::new();
        for _ in 0..300_000 {
            sim.tick().await.unwrap();
            got.extend_from_slice(&sim.take_b());
            if got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
                break;
            }
        }
        assert_same(&got, &payload);
        assert_eq!(
            sim.a.borrow().waitsnd(),
            0,
            "seed={seed} ACK 丢失后发送端仍未清空，snd_una={}",
            sim.a.borrow().snd_una
        );
    }
}

/// 接收完数据后发送端必须能确认到最终 SN（不能残留 snd_buf）
#[tokio::test]
async fn sender_fully_acknowledged_after_delivery() {
    let mut rng = Rng::new(0xACC0_0001);
    let net = Net::loss(0.3).with_delay(6);
    let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(16, 64), net, 0xACC0_0002);
    let payload = rng.bytes(sim.a.borrow().mss as usize * 10);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    for _ in 0..300_000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() && sim.a.borrow().snd_buf.is_empty() {
            break;
        }
    }
    assert_same(&got, &payload);
    assert!(sim.a.borrow().snd_buf.is_empty(), "交付完成后 snd_buf 仍有残留");
}

/// 单个 ACK 只能确认「una 之前」的段，不能凭 sn 顺带确认后面的段
///
/// 确认必须由 una 驱动。若按 ACK 的 sn 精确删除，一个 sn=0、una=0
/// 的包会把一段移出未确认队列，而 una 一旦越过它就等于承认了整条前缀，
/// 两种口径互相矛盾，会造成对端尚未收到的段被提前删除。
#[tokio::test]
async fn ack_is_per_segment() {
    let mut sink = Sink::new(KcpConfig::fast_mode_with_wnd(8, 32));
    // 先让对端窗口已知，否则首次 flush 只放行 1 段探路
    sink.kcp.borrow_mut().input(&common::stress::control_packet(1, KcpCmd::Wins, 32)).unwrap();
    sink.kcp.borrow_mut().send(&[1u8; 100]).unwrap();
    sink.kcp.borrow_mut().send(&[2u8; 100]).unwrap();
    sink.kcp.borrow_mut().send(&[3u8; 100]).unwrap();
    // 必须走一次 update 才能把段从 snd_queue 搬进 snd_buf
    sink.update(20).await.unwrap();
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 3, "三段都应处于在途状态");

    // 对端只收到 sn=0：ACK(una=1) 只能确认第一段
    let ack = common::stress::raw_packet(
        RawHeader {
            conv: 1,
            cmd: KcpCmd::Ack as u8,
            wnd: 1024,
            sn: 0,
            una: 1,
            ..RawHeader::default()
        },
        &[],
    );
    sink.kcp.borrow_mut().input(&ack).unwrap();
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 2, "只应确认 1 段");
    assert_eq!(sink.kcp.borrow().snd_una, 1, "una 应推进到 1");

    // 伪造「确认了 sn=2 但 una 仍为 1」的包：不得越界删除后面的段
    let fake = common::stress::raw_packet(
        RawHeader {
            conv: 1,
            cmd: KcpCmd::Ack as u8,
            wnd: 1024,
            sn: 2,
            una: 1,
            ..RawHeader::default()
        },
        &[],
    );
    sink.kcp.borrow_mut().input(&fake).unwrap();
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 2, "una 未推进时，仅凭 sn 不得删除未确认段");
    assert_eq!(sink.kcp.borrow().snd_una, 1);
}

/// 接收端按序交付后，下一个期望 SN 应严格递增
#[tokio::test]
async fn rcv_nxt_advances_strictly() {
    let sink = Sink::new(KcpConfig::stream_mode());
    for sn in 0..10u32 {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, b"ab")).unwrap();
        assert_eq!(sink.kcp.borrow().rcv_nxt, sn + 1);
    }
    let all = sink.kcp.borrow_mut().recv().unwrap();
    assert_eq!(all.len(), 20);
    assert_eq!(sink.kcp.borrow().rcv_nxt, 10);
}

/// 乱序到达：缺口补齐前不推进 rcv_nxt，补齐后一次性推进
#[tokio::test]
async fn gap_holds_back_until_filled() {
    let sink = Sink::new(KcpConfig::stream_mode());
    for sn in [2u32, 3, 4] {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, b"x")).unwrap();
    }
    assert_eq!(sink.kcp.borrow().rcv_nxt, 0, "缺口未补不应推进");
    assert_eq!(sink.kcp.borrow().nrcv_buf, 3);

    for sn in [0u32, 1] {
        sink.kcp.borrow_mut().input(&push_packet(1, sn, b"x")).unwrap();
    }
    assert_eq!(sink.kcp.borrow().rcv_nxt, 5, "补齐后应一次性推进");
    assert_eq!(sink.kcp.borrow().nrcv_buf, 0);
    assert_eq!(sink.kcp.borrow().nrcv_que, 5);
}

/// 窗口满时接收端应通告 0 窗口，释放后恢复通告
#[tokio::test]
async fn window_advertisement_follows_queue() {
    let cfg = KcpConfig { rcv_wnd: 2, ..KcpConfig::stream_mode() };
    let mut sink = Sink::new(cfg);

    sink.kcp.borrow_mut().input(&push_packet(1, 0, b"a")).unwrap();
    sink.update(20).await.unwrap();
    // 读走数据后回到空闲，应答通告非零窗口
    sink.kcp.borrow_mut().recv().unwrap();
    sink.clear();
    sink.kcp.borrow_mut().input(&push_packet(1, 1, b"a")).unwrap();
    sink.update(40).await.unwrap();
    let wnds: Vec<u16> = sink.emitted().iter().map(|p| u16::from_be_bytes([p[6], p[7]])).collect();
    assert!(wnds.iter().all(|&w| w > 0), "空闲后通告窗口应大于 0，实际 {wnds:?}");
}

/// 快速重传触发后，被标记的段必须真的重新出现在线路上
#[tokio::test]
async fn fast_retransmit_emits_again() {
    let net = Net::loss(0.6).with_delay(5);
    let mut sim = Sim::new(
        KcpConfig { fastresend: 2, ..KcpConfig::fast_mode_with_wnd(16, 64) },
        net,
        0xF00D_0001,
    );
    let payload = Rng::new(1).bytes(sim.a.borrow().mss as usize * 5);
    sim.a.borrow_mut().send(&payload).unwrap();

    let mut got = Vec::new();
    let mut retransmits = 0u32;
    let mut last_xmit = 0u32;
    for _ in 0..200_000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        let xmit = sim.a.borrow().xmit;
        if xmit > last_xmit {
            retransmits += xmit - last_xmit;
            last_xmit = xmit;
        }
        if got.len() >= payload.len() && sim.a.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert_same(&got, &payload);
    assert!(retransmits > 5, "高丢包下应有明显重传，实际发包 {retransmits} 次");
}

/// 更新间隔内重复调用 update 不应产生额外发包
#[tokio::test]
async fn update_is_idempotent_within_interval() {
    let mut sink = Sink::new(KcpConfig { interval: 50, ..KcpConfig::stream_mode() });
    sink.kcp.borrow_mut().send(b"data").unwrap();
    sink.update(10).await.unwrap();
    let after_first = sink.emitted().len();
    assert!(after_first >= 1);

    for t in 11..50 {
        sink.update(t).await.unwrap();
    }
    assert_eq!(sink.emitted().len(), after_first, "间隔内不应重复发包");

    sink.update(60).await.unwrap();
    assert!(sink.emitted().len() >= after_first);
}

/// 空闲连接不应持续发数据包，只允许按退避发窗口探测
#[tokio::test]
async fn idle_update_emits_no_data() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    for t in (0..5000).step_by(20) {
        sink.update(t).await.unwrap();
    }
    assert_eq!(sink.count_cmd(KcpCmd::Push), 0, "空闲连接不应发送数据");
    assert_eq!(sink.count_cmd(KcpCmd::Ack), 0, "空闲连接不应发送 ACK");
    // 对端未知时探测应按指数退避，5000ms 内远少于每 20ms 一次（250 次）
    let probes = sink.count_cmd(KcpCmd::Wask);
    assert!(probes < 20, "窗口探测未退避，5000ms 内发出 {probes} 次");
}

/// check() 在等待窗口探测时应返回探测时刻，而不是每轮都返回当前时刻
#[tokio::test]
async fn check_waits_for_probe_deadline() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    sink.update(20).await.unwrap();
    let ts_probe = sink.kcp.borrow().ts_probe;
    let next = sink.kcp.borrow().check(20);
    assert!(ts_probe > 20, "首次探测后应安排下一次探测时刻");
    assert!(next >= ts_probe, "check() 返回 {next}，早于探测时刻 {ts_probe}，调用方会空转");
}

/// ACK 包里的 sn 与 una 互相矛盾时，不得越过 una 删除段
///
/// 若既按 ACK 包序号精确删除一段、又按 una 推进确认，两条路径各自生效时，
/// 一个「sn=3、una=1」的包会把 sn=1..3 全部移出未确认队列，
/// 而 una=1 只代表对端收到了 sn=0 一段 —— 另外两段对端根本没收到，
/// 却从此不再重传。
#[tokio::test]
async fn ack_sn_must_not_delete_beyond_una() {
    // nocwnd 让 4 段可以一次性进入在途队列
    let cfg = KcpConfig { nocwnd: true, ..KcpConfig::fast_mode_with_wnd(64, 128) };
    let mut sink = Sink::new(cfg);
    // 先让对端窗口已知，否则首轮 flush 只放行 1 段探路
    sink.kcp.borrow_mut().input(&common::stress::control_packet(1, KcpCmd::Wins, 256)).unwrap();
    let mss = sink.kcp.borrow().mss as usize;
    sink.kcp.borrow_mut().send(&vec![0xABu8; mss * 4]).unwrap();
    sink.update(20).await.unwrap();
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 4, "4 段都应处于在途状态");

    // ACK 的 sn 指向 sn=3，但 una=1 只承认 sn=0 已被收到
    let ack = common::stress::raw_packet(
        RawHeader {
            conv: 1,
            cmd: KcpCmd::Ack as u8,
            wnd: 1024,
            sn: 3,
            una: 1,
            ..RawHeader::default()
        },
        &[],
    );
    sink.kcp.borrow_mut().input(&ack).unwrap();

    let sns: Vec<u32> = sink.kcp.borrow().snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
    assert_eq!(sink.kcp.borrow().snd_una, 1, "una 应按包内字段推进");
    assert_eq!(sns, vec![1, 2, 3], "只有 una 之前的段能被确认，sn=1,2 必须继续等待重传");
    assert_eq!(sink.kcp.borrow().nsnd_buf, 3, "计数必须与队列长度一致");
}

/// una 不推进时，单独的 ACK 不应让队列凭空变短
///
/// 逐段确认但 una 恒为 0 的包序列在真实链路上并不少见（接收端乱序，
/// 只有前面出现缺口时才回填 una）。修复前每收一个这样的 ACK 就少一段，
/// 到最后 snd_buf 被清空、snd_una 却仍是 0，收发双方对不上账。
#[tokio::test]
async fn ack_without_una_does_not_shrink_queue() {
    let cfg = KcpConfig { nocwnd: true, ..KcpConfig::fast_mode_with_wnd(64, 128) };
    let mut sink = Sink::new(cfg);
    sink.kcp.borrow_mut().input(&common::stress::control_packet(1, KcpCmd::Wins, 256)).unwrap();
    let mss = sink.kcp.borrow().mss as usize;
    sink.kcp.borrow_mut().send(&vec![0xCDu8; mss * 4]).unwrap();
    sink.update(20).await.unwrap();
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 4);

    for sn in 0..4u32 {
        let ack = common::stress::raw_packet(
            RawHeader { conv: 1, cmd: KcpCmd::Ack as u8, wnd: 1024, sn, ..RawHeader::default() },
            &[],
        );
        sink.kcp.borrow_mut().input(&ack).unwrap();
    }
    assert_eq!(sink.kcp.borrow().snd_una, 0, "una 从未推进");
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 4, "una 未推进时，未确认段不得被逐一删除");
    assert_eq!(sink.kcp.borrow().nsnd_buf, 4);
}
