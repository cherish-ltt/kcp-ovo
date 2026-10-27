//! 黑盒测试：生命周期、状态查询与长时间运行
//!
//! 覆盖 waitsnd / check / update 的返回约定，以及长时间持续传输不累积泄漏。

mod common;

use common::{Net, Rng, Sim, Solo, assert_same};
use kcp_ovo::error::KcpErrorKind;
use kcp_ovo::{KcpConfig, KcpError};

/// 全新实例的初始状态应符合公开文档的约定
#[test]
fn fresh_instance_state() {
    let kcp = kcp_ovo::core::kcp::Kcp::new(0x1234, KcpConfig::default()).unwrap();
    assert_eq!(kcp.conv, 0x1234);
    assert_eq!(kcp.mtu, 1400);
    assert_eq!(kcp.mss, 1400 - 28);
    assert_eq!(kcp.snd_una, 0);
    assert_eq!(kcp.snd_nxt, 0);
    assert_eq!(kcp.rcv_nxt, 0);
    assert_eq!(kcp.waitsnd(), 0);
    assert!(kcp.snd_wnd > 0);
    assert!(kcp.rcv_wnd > 0);
    assert!(kcp.rx_rto >= 100, "初始 RTO 不应小于最小值");
}

/// waitsnd 应随入队/确认变化，反映“还有多少没结束”
#[tokio::test]
async fn waitsnd_tracks_outstanding() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 61);
    let mss = sim.a.borrow().mss as usize;
    sim.a.borrow_mut().send(&vec![0u8; mss * 5]).unwrap();
    assert_eq!(sim.a.borrow().waitsnd(), 5, "刚入队时 5 段都算未结束");

    sim.run(2000).await.unwrap();
    assert_eq!(sim.a.borrow().waitsnd(), 0, "全部确认后应归零");
}

/// 空队列时 recv / peeksize 返回明确的错误
#[test]
fn recv_empty_returns_error() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    assert!(matches!(kcp.recv(), Err(KcpError::QueueEmpty)));
    assert!(kcp.peeksize().is_err());
}

/// 未设置 output 时发送应报 OutputNotSet 而不是 panic
#[tokio::test]
async fn send_without_output_errors() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::stream_mode()).unwrap();
    kcp.send(b"data").unwrap();
    let err = kcp.update(100).await.unwrap_err();
    assert_eq!(err.kind(), KcpErrorKind::OutputNotSet);
}

/// check() 返回的时间点应推进到未来，可用于安排下一次 update
#[test]
fn check_returns_future_deadline() {
    let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, KcpConfig::fast_mode()).unwrap();
    kcp.current = 1000;
    kcp.ts_flush = 1000;
    kcp.snd_buf.clear();
    kcp.nsnd_buf = 0;
    let next = kcp.check(1000);
    assert!(next >= 1000, "check 不应返回过去的时间点");
}

/// 长时间持续传输（模拟 500 轮请求）后，队列不应持续增长
#[tokio::test]
async fn no_queue_growth_over_long_session() {
    let mut sim = Sim::new(KcpConfig::stream_mode(), Net::default(), 62);
    for round in 0..500u32 {
        let part = Rng::new(round as u64 + 7).bytes(200);
        sim.a.borrow_mut().send(&part).unwrap();
        let got = sim.drain_a_to_b(200).await.unwrap();
        assert_same(&got, &part);
    }
    assert_eq!(sim.a.borrow().nsnd_buf, 0, "发送缓冲应在每轮清空");
    assert_eq!(sim.a.borrow().nsnd_que, 0, "发送队列应在每轮清空");
    assert_eq!(sim.b.borrow().nrcv_que, 0, "接收队列应被读空");
    assert_eq!(sim.a.borrow().waitsnd(), 0);
}

/// 接收端长时间不读数据时，发送端应因窗口耗尽而暂停，而不是无限膨胀
#[tokio::test]
async fn sender_pauses_when_receiver_stalls() {
    let cfg = KcpConfig { rcv_wnd: 16, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, Net::default(), 63);
    let mss = sim.a.borrow().mss as usize;
    sim.a.borrow_mut().send(&vec![1u8; mss * 100]).unwrap();

    sim.run(5000).await.unwrap();
    // 接收端从不 recv，发送端最多能塞满对端窗口
    assert!(
        sim.a.borrow().nsnd_buf <= 17,
        "接收端停读时在途段数 {} 超出对端窗口 16",
        sim.a.borrow().nsnd_buf
    );

    // 一旦开始读，传输应恢复并完成
    let mut got = Vec::new();
    for _ in 0..20000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= mss * 100 {
            break;
        }
    }
    assert_eq!(got.len(), mss * 100, "恢复读取后应传完全部数据");
}

/// update 的主要逻辑在 ts_flush 到期时才执行，两次 update 之间不应重复刷新
#[tokio::test]
async fn update_throttled_by_interval() {
    let mut solo = Solo::new(KcpConfig::fast_mode());
    solo.kcp.borrow_mut().send(b"x").unwrap();
    // 第一次 update 建立基线并发包
    let n1 = solo.step(1).await.unwrap();
    // 首轮会同时发出数据段与窗口探测（对端窗口尚不可知），因此只断言有输出
    assert!(n1 >= 1, "首次 update 应发出数据段");
    // 紧接着的 update 未到 interval，不应重复发送
    let n2 = solo.step(1).await.unwrap();
    assert_eq!(n2, 0, "interval 内不应再次刷新");
    // 窗口探测按 IKCP_PROBE_INIT 退避，interval 到期只应触发 flush，
    // 不应额外产生探测帧；这里只断言仍未重新探测
    let n3 = solo.step(50).await.unwrap();
    assert_eq!(n3, 0, "探测退避未到期时不应再发探测帧");
    // 跨过首次探测等待后应重新发出探测
    let wait = solo.kcp.borrow().probe_wait;
    assert!(wait >= kcp_ovo::helper::IKCP_PROBE_INIT, "首次探测等待应不小于 IKCP_PROBE_INIT");
    let n4 = solo.step(wait).await.unwrap();
    assert!(n4 >= 1, "探测等待到期后应重新发出探测帧");
}

/// 单端连续 update 不应 panic，即使从未收到任何包
#[tokio::test]
async fn update_without_peer_is_safe() {
    let mut solo = Solo::new(KcpConfig::stream_mode());
    for _ in 0..500 {
        solo.step(20).await.unwrap();
    }
    assert_eq!(solo.kcp.borrow().nsnd_que, 0);
}

/// 接收端收到的乱序段应暂存在 rcv_buf，按序到位后一次转入 rcv_queue
#[test]
fn out_of_order_segments_buffered_then_released() {
    let mut kcp =
        kcp_ovo::core::kcp::Kcp::new(1, KcpConfig { rcv_wnd: 64, ..KcpConfig::stream_mode() })
            .unwrap();
    // 先送 sn=1,2，sn=0 缺失 → 均应留在 rcv_buf
    for sn in [1u32, 2] {
        kcp.input(&make_packet(1, sn, b"data")).unwrap();
    }
    assert_eq!(kcp.nrcv_que, 0, "缺 sn=0 时不应交付");
    assert_eq!(kcp.nrcv_buf, 2, "乱序段应暂存在 rcv_buf");

    // 补齐 sn=0 → 三段一起交付
    kcp.input(&make_packet(1, 0, b"data")).unwrap();
    assert_eq!(kcp.nrcv_que, 3, "补齐后应一次性转入 rcv_queue");
    assert_eq!(kcp.nrcv_buf, 0);

    let all = kcp.recv().unwrap();
    assert_eq!(all.len(), 12, "流模式应拼接全部三段");
}

/// 重复收到同一个段不应让 rcv_queue 增长
#[test]
fn duplicate_segment_not_delivered_twice() {
    let mut kcp =
        kcp_ovo::core::kcp::Kcp::new(1, KcpConfig { rcv_wnd: 64, ..KcpConfig::stream_mode() })
            .unwrap();
    kcp.input(&make_packet(1, 0, b"abc")).unwrap();
    kcp.input(&make_packet(1, 0, b"abc")).unwrap();
    kcp.input(&make_packet(1, 0, b"abc")).unwrap();
    assert_eq!(kcp.nrcv_que, 1, "重复段只能入队一次");
    assert_same(&kcp.recv().unwrap(), b"abc");
    assert!(kcp.recv().is_err(), "不应有第二次交付");
}

/// 远端窗口更新后发送端应立即恢复放量
#[tokio::test]
async fn reopening_window_resumes_transfer() {
    let cfg = KcpConfig { rcv_wnd: 4, ..KcpConfig::stream_mode() };
    let mut sim = Sim::new(cfg, Net::default(), 64);
    let mss = sim.a.borrow().mss as usize;
    let payload = Rng::new(64).bytes(mss * 30);
    sim.a.borrow_mut().send(&payload).unwrap();

    // 先堵住接收端
    sim.run(3000).await.unwrap();
    let mut got = sim.take_b();
    let stalled = got.len();

    // 打开接收端并持续读取
    for _ in 0..40000 {
        sim.tick().await.unwrap();
        got.extend_from_slice(&sim.take_b());
        if got.len() >= payload.len() {
            break;
        }
    }
    assert!(stalled < payload.len(), "初始阶段确实被窗口卡住");
    assert_same(&got, &payload);
}

fn make_packet(conv: u32, sn: u32, payload: &[u8]) -> Vec<u8> {
    let mut raw = vec![0u8; 28 + payload.len()];
    raw[0..4].copy_from_slice(&conv.to_be_bytes());
    raw[4] = 81;
    raw[6..8].copy_from_slice(&1024u16.to_be_bytes());
    raw[16..20].copy_from_slice(&sn.to_be_bytes());
    raw[24..28].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    raw[28..].copy_from_slice(payload);
    raw
}
