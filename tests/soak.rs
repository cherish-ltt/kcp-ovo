//! 黑盒测试：大规模、长时间压力
//!
//! 前面的用例以“够用就好”的规模验证正确性。这里把数据量、时长、往返次数
//! 都拉到接近真实生产的量级，专门用来暴露只有在长跑中才会显现的问题：
//! SN 回绕、队列碎片、内存无界增长、状态漂移。
//!
//! 全部使用虚拟时钟，因此“跑一分钟”只需毫秒级 CPU 时间；随机源固定种子。

mod common;

use common::stress::{Sink, transfer_a_to_b, transfer_both};
use common::{Net, Rng, Sim, assert_same, checksum};
use kcp_ovo::KcpConfig;
use kcp_ovo::helper::KcpCmd;

/// 单次 1MB 流式传输（约 765 个分片）
#[tokio::test]
async fn bulk_transfer_one_megabyte() {
    let mut rng = Rng::new(0xB001_0001);
    let net = Net::loss(0.05).with_delay(8).with_jitter(4);
    let cfg = KcpConfig { rcv_wnd: 512, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0xB001_0100);
    let payload = rng.bytes(1024 * 1024);

    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 2_000_000).await;
    assert!(ok, "1MB 传输未完成，{ticks} tick");
    assert_same(&got, &payload);
}

/// 4MB 大文件在 10% 丢包下仍应完整送达
#[tokio::test]
async fn bulk_transfer_four_megabytes_lossy() {
    let mut rng = Rng::new(0xB001_0002);
    let net = Net::loss(0.1).with_delay(10).with_jitter(5).with_reorder(0.1, 0.05);
    let cfg = KcpConfig { rcv_wnd: 512, snd_wnd: 512, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0xB001_0200);
    let payload = rng.bytes(4 * 1024 * 1024);

    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 8_000_000).await;
    assert!(ok, "4MB 丢包传输未完成，{ticks} tick");
    assert_eq!(got.len(), payload.len(), "总长度不一致");
    assert_eq!(checksum(&got), checksum(&payload), "内容校验失败");
}

/// 长会话：1000 轮双向小消息，验证状态不随轮次漂移
#[tokio::test]
async fn long_session_thousand_rounds() {
    let mut rng = Rng::new(0xB001_0003);
    let net = Net::loss(0.02).with_delay(5);
    let mut sim = Sim::new(KcpConfig::fast_mode_with_wnd(64, 128), net, 0xB001_0300);

    for round in 0..1000u64 {
        let a_payload = rng.bytes(40);
        let b_payload = rng.bytes(40);
        let (ok, got_a, got_b, ticks) =
            transfer_both(&mut sim, &a_payload, &b_payload, 200_000).await;
        assert!(ok, "第 {round} 轮未完成，{ticks} tick");
        assert_same(&got_a, &a_payload);
        assert_same(&got_b, &b_payload);
        // 每轮结束后两端都不应残留
        assert_eq!(sim.a.borrow().waitsnd(), 0, "第 {round} 轮 A 残留待发数据");
        assert_eq!(sim.b.borrow().waitsnd(), 0, "第 {round} 轮 B 残留待发数据");
    }
}

/// 连续传输 200 条消息，边界与顺序都必须保持
#[tokio::test]
async fn many_messages_keep_boundaries() {
    let mut rng = Rng::new(0xB001_0004);
    let net = Net::loss(0.08).with_delay(6);
    let cfg = KcpConfig { rcv_wnd: 256, ..KcpConfig::fast_mode_with_wnd(64, 256) };
    let mut sim = Sim::new(cfg, net, 0xB001_0400);
    let mss = sim.a.borrow().mss as usize;

    let mut sent: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    for _ in 0..200 {
        // 每条 1..=4 段，确保单条不会灌满接收窗口
        let msg = rng.bytes_segments(mss, 4);
        total += msg.len();
        sim.a.borrow_mut().send(&msg).unwrap();
        sent.push(msg);
    }

    let mut got_msgs: Vec<Vec<u8>> = Vec::new();
    let mut got_bytes = 0usize;
    for _ in 0..2_000_000 {
        sim.tick().await.unwrap();
        for m in sim.b_messages() {
            got_bytes += m.len();
            got_msgs.push(m);
        }
        if got_bytes >= total && sim.a.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert_eq!(got_msgs.len(), sent.len(), "消息条数不一致");
    for (i, (g, s)) in got_msgs.iter().zip(sent.iter()).enumerate() {
        assert_eq!(checksum(g), checksum(s), "第 {i} 条消息内容不一致");
        assert_eq!(g.len(), s.len(), "第 {i} 条消息长度不一致");
    }
}

/// SN 接近 u32 回绕点时传输仍然正确
///
/// KCP 用 wrapping 减法比较序号，回绕是正常场景而非异常。
#[tokio::test]
async fn sequence_wraparound_is_transparent() {
    let mut rng = Rng::new(0xB001_0005);
    for start in [u32::MAX - 30, u32::MAX - 5, u32::MAX] {
        let net = Net::loss(0.05).with_delay(4);
        let mut sim = Sim::new(KcpConfig::stream_mode(), net, 0xB001_0500);
        sim.a.borrow_mut().snd_nxt = start;
        sim.a.borrow_mut().snd_una = start;
        sim.b.borrow_mut().rcv_nxt = start;

        let payload = rng.bytes(sim.a.borrow().mss as usize * 40);
        let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 1_000_000).await;
        assert!(ok, "起点 {start} 附近回绕失败，{ticks} tick");
        assert_same(&got, &payload);
    }
}

/// 极端网络：40% 丢包 + 抖动 + 乱序 + 重复，长时间仍必须收敛
#[tokio::test]
async fn hostile_network_still_converges() {
    let mut rng = Rng::new(0xB001_0006);
    let net = Net::loss(0.4).with_delay(12).with_jitter(10).with_reorder(0.3, 0.2);
    // 用流模式：消息模式下单条消息分片数超过 rcv_wnd 会按协议永久阻塞
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 128, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0xB001_0600);
    let payload = rng.bytes(sim.a.borrow().mss as usize * 200);

    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 4_000_000).await;
    assert!(ok, "恶劣网络下未收敛，{ticks} tick");
    assert_same(&got, &payload);
}

/// 高延迟长肥管道：200ms RTT、8MB 窗口内应正常传输
#[tokio::test]
async fn long_fat_pipe_transfer() {
    let mut rng = Rng::new(0xB001_0007);
    let net = Net::loss(0.01).with_delay(100).with_jitter(10);
    let cfg = KcpConfig { snd_wnd: 1024, rcv_wnd: 1024, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0xB001_0700);
    let payload = rng.bytes(512 * 1024);

    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 2_000_000).await;
    assert!(ok, "高延迟链路未完成，{ticks} tick");
    assert_same(&got, &payload);
}

/// 空闲期反复探测后，重新发数据必须立即可用
#[tokio::test]
async fn idle_probe_then_resume() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 0xB001_0800);
    let mss = sim.a.borrow().mss as usize;

    // 先传一轮建立连接
    let first = Rng::new(1).bytes(mss * 2);
    let (ok, got, _) = transfer_a_to_b(&mut sim, &first, 100_000).await;
    assert!(ok);
    assert_same(&got, &first);

    // 空闲 30 秒：期间只有窗口探测
    for _ in 0..30_000 {
        sim.tick().await.unwrap();
        sim.take_a();
        sim.take_b();
    }

    // 空闲后继续传输
    let second = Rng::new(2).bytes(mss * 2);
    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &second, 100_000).await;
    assert!(ok, "空闲 30 秒后恢复传输失败，{ticks} tick");
    assert_same(&got, &second);
}

/// 十万 tick 的长跑中，两端的内部队列都不应无界增长
#[tokio::test]
async fn queues_stay_bounded_over_time() {
    let mut rng = Rng::new(0xB001_0009);
    let net = Net::loss(0.2).with_delay(8).with_jitter(4);
    let mut sim = Sim::new(
        KcpConfig { rcv_wnd: 64, ..KcpConfig::fast_mode_with_wnd(32, 64) },
        net,
        0xB001_0900,
    );
    let mss = sim.a.borrow().mss as usize;

    let mut max_snd = 0usize;
    let mut max_rcv = 0usize;
    for round in 0..20 {
        let payload = rng.bytes(mss * 8);
        sim.a.borrow_mut().send(&payload).unwrap();
        for _ in 0..5000 {
            sim.tick().await.unwrap();
            sim.take_b();
            max_snd = max_snd.max(sim.a.borrow().snd_buf.len());
            max_rcv = max_rcv.max(sim.b.borrow().nrcv_buf);
            if sim.a.borrow().waitsnd() == 0 {
                break;
            }
        }
        assert_eq!(sim.a.borrow().waitsnd(), 0, "第 {round} 轮未清空");
    }
    // 发送窗口 32、接收窗口 64，留出重传叠加的余量后仍必须远小于总量
    assert!(max_snd <= 64, "snd_buf 峰值 {max_snd}，疑似无界增长");
    assert!(max_rcv <= 64, "rcv_buf 峰值 {max_rcv}，超过接收窗口");
}

/// 单端一次性入队大量数据，不应触发分片数硬上限
#[tokio::test]
async fn stream_mode_accepts_huge_single_send() {
    let sink = Sink::new(KcpConfig { snd_wnd: 1024, rcv_wnd: 1024, ..KcpConfig::stream_mode() });
    let payload = Rng::new(3).bytes(2 * 1024 * 1024);
    let queued = sink.kcp.borrow_mut().send(&payload).unwrap();
    assert_eq!(queued, payload.len(), "流模式应接受全部数据");

    // 消息模式下同样数据量应被明确拒绝，而不是静默截断
    let msg_sink = Sink::new(KcpConfig { snd_wnd: 1024, rcv_wnd: 1024, ..KcpConfig::fast_mode() });
    let err = msg_sink.kcp.borrow_mut().send(&payload).unwrap_err();
    assert!(
        err.to_string().contains("Too many fragments"),
        "消息模式应明确拒绝超分片数据，实际错误：{err}"
    );
}

/// 大量 ACK 风暴下发送端状态仍保持一致
///
/// 每轮 flush 只把一批段搬进 snd_buf，因此需要「推动 + 确认」交替进行，
/// 直到全部 200 段都被确认。
#[tokio::test]
async fn ack_storm_keeps_state_consistent() {
    let mut sink = Sink::new(KcpConfig::stream_mode());
    sink.kcp.borrow_mut().input(&common::stress::control_packet(1, KcpCmd::Wins, 512)).unwrap();
    for i in 0..200u8 {
        sink.kcp.borrow_mut().send(&[i; 20]).unwrap();
    }

    for round in 0..100u64 {
        sink.update(20 + round * 20).await.unwrap();
        let in_flight: Vec<u32> =
            sink.kcp.borrow().snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        // 每个在途 SN 重复确认 50 次，模拟 ACK 风暴。
        // 确认只由 una 驱动（与 C 原版一致），因此风暴中的每个 ACK 都要带上
        // 「对端已连续收到的位置」= 该 sn + 1，否则风暴不会推进任何确认，
        // 这与真实实现里接收端回填 una = rcv_nxt 的行为对应。
        for _ in 0..50 {
            for sn in &in_flight {
                let pkt = common::stress::raw_packet(
                    common::stress::RawHeader {
                        conv: 1,
                        cmd: KcpCmd::Ack as u8,
                        wnd: 512,
                        sn: *sn,
                        una: sn.wrapping_add(1),
                        ..common::stress::RawHeader::default()
                    },
                    &[],
                );
                sink.kcp.borrow_mut().input(&pkt).unwrap();
            }
        }
        if sink.kcp.borrow().waitsnd() == 0 {
            break;
        }
    }
    assert_eq!(sink.kcp.borrow().nsnd_buf, 0, "全部确认后仍残留未确认段");
    assert_eq!(sink.kcp.borrow().snd_buf.len(), 0);
    assert_eq!(sink.kcp.borrow().waitsnd(), 0);
}

/// 回绕点上双向同时传输，接收窗口恰好跨越 u32::MAX
///
/// 既有用例把 rcv_nxt 设在回绕点附近，但两端都是从同一位置出发、窗口内也没有
/// 大量乱序段同时压住回绕点。这里让两个方向同时跨过 u32::MAX，
/// 且窗口边界本身落在回绕点两侧，验证窗口比较、乱序缓存与交付都按 wrapping 语义工作。
#[tokio::test]
async fn wraparound_with_bidirectional_window_span() {
    use common::stress::transfer_both;

    let mut rng = Rng::new(0xB001_0B01);
    for start in [u32::MAX - 2, u32::MAX - 1, u32::MAX] {
        let net = Net::loss(0.08).with_delay(6).with_jitter(3).with_reorder(0.15, 0.05);
        let cfg = KcpConfig { snd_wnd: 16, rcv_wnd: 32, ..KcpConfig::stream_mode() };
        let mut sim = Sim::new(cfg, net, 0xB001_0B00);
        sim.a.borrow_mut().snd_nxt = start;
        sim.a.borrow_mut().snd_una = start;
        sim.b.borrow_mut().rcv_nxt = start;
        // 反方向也跨同一个回绕点，验证两端独立推进互不干扰
        sim.b.borrow_mut().snd_nxt = start;
        sim.b.borrow_mut().snd_una = start;
        sim.a.borrow_mut().rcv_nxt = start;

        let mss = sim.a.borrow().mss as usize;
        let a_payload = rng.bytes(mss * 24);
        let b_payload = rng.bytes(mss * 24);
        let (ok, got_b, got_a, ticks) =
            transfer_both(&mut sim, &a_payload, &b_payload, 2_000_000).await;
        assert!(ok, "起点 {start} 双向回绕失败，{ticks} tick");
        assert_same(&got_b, &a_payload);
        assert_same(&got_a, &b_payload);
        assert_eq!(sim.a.borrow().waitsnd(), 0, "A 端应全部确认");
        assert_eq!(sim.b.borrow().waitsnd(), 0, "B 端应全部确认");
    }
}

/// 回绕点后立刻重传乱序段：旧段与回绕后的新段必须被正确区分
///
/// 回绕点附近最容易出现的错误是比较号把「回绕后的大序号」当成过期段。
/// 这里人为制造一次丢包与乱序，让回绕两侧的段同时滞留在 rcv_buf。
#[tokio::test]
async fn wraparound_mixed_with_retransmission() {
    let start = u32::MAX - 1;
    let net = Net::loss(0.2).with_delay(8).with_reorder(0.25, 0.1);
    let cfg = KcpConfig { snd_wnd: 8, rcv_wnd: 32, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, net, 0xB001_0C00);
    sim.a.borrow_mut().snd_nxt = start;
    sim.a.borrow_mut().snd_una = start;
    sim.b.borrow_mut().rcv_nxt = start;

    let mut rng = Rng::new(0xB001_0C01);
    let mss = sim.a.borrow().mss as usize;
    let payload = rng.bytes(mss * 32);
    let (ok, got, ticks) = transfer_a_to_b(&mut sim, &payload, 3_000_000).await;
    assert!(ok, "回绕 + 乱序 + 丢包未收敛，{ticks} tick");
    assert_same(&got, &payload);
    assert_eq!(sim.b.borrow().nrcv_buf, 0, "交付完成后不应残留乱序缓存");
}
