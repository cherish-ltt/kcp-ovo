//! 黑盒测试：多客户端并发
//!
//! 前面所有用例都是「两端互连」。真实服务端要同时服务几十上百个客户端，
//! 每个客户端有独立的 conv、独立的窗口状态和独立的 RTO。
//! 这里把多个 `Kcp` 实例挂到同一个虚拟网络上交错推进，验证：
//! - 各连接的 conv 不会串台
//! - 单条连接丢包或断网不影响其它连接
//! - 大量连接同时传输时数据仍然逐字节正确
//! - 连接之间的 ACK 不会被错配

// 多连接共享同一批 Kcp 实例，跨 await 持有借用是有意的：
// 所有连接都在同一个线程上串行推进，await 立即完成，不存在并发重入。
#![allow(clippy::await_holding_refcell_ref)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kcp_ovo::KcpConfig;
use kcp_ovo::KcpError;
use kcp_ovo::core::kcp::Kcp;

use common::{Rng, checksum};

/// 一条虚拟连接：服务端 + 客户端，各自的输出队列
struct Conn {
    server: Rc<RefCell<Kcp>>,
    client: Rc<RefCell<Kcp>>,
    c2s: Arc<Mutex<Vec<Vec<u8>>>>,
    s2c: Arc<Mutex<Vec<Vec<u8>>>>,
    /// 该连接的链路参数，允许每个客户端各不相同
    loss: f64,
    down: bool,
}

impl Conn {
    fn new(conv: u32, cfg_s: KcpConfig, cfg_c: KcpConfig) -> Self {
        let c2s: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let s2c: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

        let server = Rc::new(RefCell::new(Kcp::new(conv, cfg_s).expect("server")));
        let client = Rc::new(RefCell::new(Kcp::new(conv, cfg_c).expect("client")));

        {
            let sink = s2c.clone();
            server.borrow_mut().set_output(move |data| {
                let sink = sink.clone();
                Box::pin(async move {
                    sink.lock().unwrap().push(data.to_vec());
                    Ok::<usize, KcpError>(data.len())
                })
            });
        }
        {
            let sink = c2s.clone();
            client.borrow_mut().set_output(move |data| {
                let sink = sink.clone();
                Box::pin(async move {
                    sink.lock().unwrap().push(data.to_vec());
                    Ok::<usize, KcpError>(data.len())
                })
            });
        }

        Self { server, client, c2s, s2c, loss: 0.0, down: false }
    }
}

/// 多连接虚拟网络：所有连接共享同一个时钟
struct MultiNet {
    conns: Vec<Conn>,
    now: u64,
    rng: Rng,
    /// 固定单程时延，按 `arrive_at` 排队，不在同一 tick 内往返
    delay: u64,
    /// 在途包：`(到达时刻, 连接下标, 是否发往服务端, 包体)`
    inflight: Vec<(u64, usize, bool, Vec<u8>)>,
}

impl MultiNet {
    fn new(count: usize, cfg: KcpConfig, delay: u64, seed: u64) -> Self {
        let conns = (0..count).map(|i| Conn::new(i as u32 + 1, cfg.clone(), cfg.clone())).collect();
        Self { conns, now: 0, rng: Rng::new(seed), delay, inflight: Vec::new() }
    }

    /// 推进一个 tick：所有连接先 flush，再按各自丢包率投递
    async fn tick(&mut self) -> Result<(), KcpError> {
        self.now += 1;
        let now = self.now;

        for conn in self.conns.iter() {
            conn.server.borrow_mut().update(now).await?;
            conn.client.borrow_mut().update(now).await?;
        }

        for (idx, conn) in self.conns.iter_mut().enumerate() {
            let from_client = std::mem::take(&mut *conn.c2s.lock().unwrap());
            let from_server = std::mem::take(&mut *conn.s2c.lock().unwrap());
            if conn.down {
                continue;
            }
            let loss = conn.loss;
            for pkt in from_client {
                if self.rng.chance(loss) {
                    continue;
                }
                self.inflight.push((now + self.delay, idx, true, pkt));
            }
            for pkt in from_server {
                if self.rng.chance(loss) {
                    continue;
                }
                self.inflight.push((now + self.delay, idx, false, pkt));
            }
        }

        // 投递到期的包
        let now = self.now;
        let mut ready = Vec::new();
        let mut keep = Vec::new();
        for (arrive_at, idx, to_server, pkt) in self.inflight.drain(..) {
            if arrive_at <= now {
                ready.push((idx, to_server, pkt));
            } else {
                keep.push((arrive_at, idx, to_server, pkt));
            }
        }
        self.inflight = keep;
        for (idx, to_server, pkt) in ready {
            let conn = &self.conns[idx];
            // 断网连接的在途包直接丢弃
            if conn.down {
                continue;
            }
            let target = if to_server { &conn.server } else { &conn.client };
            let _ = target.borrow_mut().input(&pkt);
        }
        Ok(())
    }
}

/// 20 个客户端并发上传，每个客户端数据都必须完整且互不串台
#[tokio::test]
async fn twenty_clients_upload_concurrently() {
    const CLIENTS: usize = 20;
    let mut rng = Rng::new(0xD00D_0001);
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 128, ..KcpConfig::stream_mode() };
    let mut net = MultiNet::new(CLIENTS, cfg, 3, 0xD00D_0100);

    let mss = net.conns[0].client.borrow().mss as usize;
    let mut sent: Vec<Vec<u8>> = Vec::new();
    for (i, conn) in net.conns.iter().enumerate() {
        // 每个客户端数据不同，便于检测串台
        let mut payload = rng.bytes(mss * (10 + i));
        payload[0] = i as u8;
        conn.client.borrow_mut().send(&payload).unwrap();
        sent.push(payload);
    }

    let mut got: Vec<Vec<u8>> = vec![Vec::new(); CLIENTS];
    for _ in 0..2_000_000 {
        net.tick().await.unwrap();
        for (i, conn) in net.conns.iter().enumerate() {
            if let Ok(m) = conn.server.borrow_mut().recv() {
                got[i].extend_from_slice(&m);
            }
        }
        let all_done = net
            .conns
            .iter()
            .zip(got.iter())
            .zip(sent.iter())
            .all(|((c, g), s)| g.len() >= s.len() && c.client.borrow().waitsnd() == 0);
        if all_done {
            break;
        }
    }

    for (i, (g, s)) in got.iter().zip(sent.iter()).enumerate() {
        assert_eq!(g.len(), s.len(), "客户端 {i} 长度不一致");
        assert_eq!(checksum(g), checksum(s), "客户端 {i} 内容不一致");
    }
}

/// 部分客户端链路中断，其余客户端不受影响
#[tokio::test]
async fn one_client_down_does_not_affect_others() {
    const CLIENTS: usize = 8;
    let mut rng = Rng::new(0xD00D_0002);
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 128, ..KcpConfig::stream_mode() };
    let mut net = MultiNet::new(CLIENTS, cfg, 3, 0xD00D_0200);

    let mss = net.conns[0].client.borrow().mss as usize;
    let mut sent: Vec<Vec<u8>> = Vec::new();
    for (i, conn) in net.conns.iter_mut().enumerate() {
        let payload = rng.bytes(mss * 30);
        conn.client.borrow_mut().send(&payload).unwrap();
        sent.push(payload);
        // 一半的客户端断网
        if i % 2 == 0 {
            conn.down = true;
        }
    }
    let mut got: Vec<Vec<u8>> = vec![Vec::new(); CLIENTS];
    for _ in 0..1_000_000 {
        net.tick().await.unwrap();
        for (i, conn) in net.conns.iter().enumerate() {
            if let Ok(m) = conn.server.borrow_mut().recv() {
                got[i].extend_from_slice(&m);
            }
        }
        let healthy_done = net
            .conns
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 1)
            .all(|(i, _)| got[i].len() >= sent[i].len());
        if healthy_done {
            break;
        }
    }

    for i in (1..CLIENTS).step_by(2) {
        assert_eq!(checksum(&got[i]), checksum(&sent[i]), "健康客户端 {i} 数据不正确");
    }
    for i in (0..CLIENTS).step_by(2) {
        assert!(got[i].is_empty(), "断网客户端 {i} 不应收到数据");
    }
}

/// 不同丢包率混跑：高丢包客户端拖慢但不能带偏低丢包客户端
#[tokio::test]
async fn mixed_loss_rates_are_isolated() {
    const CLIENTS: usize = 10;
    let mut rng = Rng::new(0xD00D_0003);
    let cfg = KcpConfig { snd_wnd: 32, rcv_wnd: 128, ..KcpConfig::stream_mode() };
    let mut net = MultiNet::new(CLIENTS, cfg, 3, 0xD00D_0300);

    let mss = net.conns[0].client.borrow().mss as usize;
    let mut sent: Vec<Vec<u8>> = Vec::new();
    for (i, conn) in net.conns.iter_mut().enumerate() {
        // 交替 0% / 30% 丢包
        conn.loss = if i % 2 == 0 { 0.0 } else { 0.3 };
        let payload = rng.bytes(mss * 20);
        conn.client.borrow_mut().send(&payload).unwrap();
        sent.push(payload);
    }

    let mut got: Vec<Vec<u8>> = vec![Vec::new(); CLIENTS];
    for _ in 0..3_000_000 {
        net.tick().await.unwrap();
        for (i, conn) in net.conns.iter().enumerate() {
            if let Ok(m) = conn.server.borrow_mut().recv() {
                got[i].extend_from_slice(&m);
            }
        }
        if net
            .conns
            .iter()
            .enumerate()
            .all(|(i, c)| got[i].len() >= sent[i].len() && c.client.borrow().waitsnd() == 0)
        {
            break;
        }
    }

    for (i, (g, s)) in got.iter().zip(sent.iter()).enumerate() {
        assert_eq!(checksum(g), checksum(s), "连接 {i} 数据不一致");
    }
}

/// 50 个连接同时建立并互发小消息，验证 conv 不串台
#[tokio::test]
async fn fifty_connections_do_not_cross_talk() {
    const CLIENTS: usize = 50;
    let mut net = MultiNet::new(CLIENTS, KcpConfig::fast_mode_with_wnd(16, 32), 1, 0xD00D_0400);

    for (i, conn) in net.conns.iter().enumerate() {
        conn.client.borrow_mut().send(&[i as u8; 8]).unwrap();
    }

    let mut got = vec![false; CLIENTS];
    for _ in 0..200_000 {
        net.tick().await.unwrap();
        for (i, conn) in net.conns.iter().enumerate() {
            if let Ok(m) = conn.server.borrow_mut().recv() {
                assert_eq!(m.len(), 8, "连接 {i} 收到长度异常的消息");
                assert!(m.iter().all(|b| *b == i as u8), "连接 {i} 收到了其它连接的数据");
                got[i] = true;
            }
        }
        if got.iter().all(|v| *v) {
            break;
        }
    }
    assert!(got.iter().all(|v| *v), "部分连接未收到数据：{got:?}");
}
