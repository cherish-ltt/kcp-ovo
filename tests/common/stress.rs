//! 压力/矩阵类黑盒用例的公共辅助
//!
//! 与 `common/mod.rs` 一样只使用公开 API。这里补充三类工具：
//! - `Rendezvous`：主动驱动双方 drain，避免延迟 ACK 造成的伪停滞
//! - `config_matrix()`：常用配置组合，用于互连矩阵
//! - 数据包构造函数：构造畸形/恶意输入

use std::cell::RefCell;
use std::rc::Rc;

use kcp_ovo::KcpConfig;
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::{IKCP_OVERHEAD, KcpCmd};

use super::{Net, Sim};

/// 主动对拍驱动器
///
/// KCP 的 ACK 是延迟发送的：接收方要等到自己的下一个 flush 才会回 ACK，
/// 因此“定时 drain”才能让 ACK 及时回流。所有带丢包/乱序的用例都应走这里，
/// 否则会把“延迟 ACK”误判成“协议停滞”。
pub struct Rendezvous<'a> {
    sim: &'a mut Sim,
    /// 需要的总字节数；0 表示只等两端 waitsnd 归零
    expect_a_to_b: usize,
    expect_b_to_a: usize,
    got_a_to_b: Vec<u8>,
    got_b_to_a: Vec<u8>,
    /// 按 recv 边界累计的消息，避免取走数据后无法再核对边界
    msg_a_to_b: Vec<Vec<u8>>,
    msg_b_to_a: Vec<Vec<u8>>,
    ticks: usize,
}

impl<'a> Rendezvous<'a> {
    pub fn new(sim: &'a mut Sim, expect_a_to_b: usize, expect_b_to_a: usize) -> Self {
        Self {
            sim,
            expect_a_to_b,
            expect_b_to_a,
            got_a_to_b: Vec::new(),
            got_b_to_a: Vec::new(),
            msg_a_to_b: Vec::new(),
            msg_b_to_a: Vec::new(),
            ticks: 0,
        }
    }

    fn collect(&mut self) {
        let a_to_b = self.sim.b_messages();
        let b_to_a = self.sim.a_messages();
        for m in a_to_b {
            self.got_a_to_b.extend_from_slice(&m);
            self.msg_a_to_b.push(m);
        }
        for m in b_to_a {
            self.got_b_to_a.extend_from_slice(&m);
            self.msg_b_to_a.push(m);
        }
    }

    fn done(&self) -> bool {
        let a_ok = self.got_a_to_b.len() >= self.expect_a_to_b;
        let b_ok = self.got_b_to_a.len() >= self.expect_b_to_a;
        let idle = self.sim.a.borrow().waitsnd() == 0 && self.sim.b.borrow().waitsnd() == 0;
        a_ok && b_ok && idle
    }

    /// 推进到条件满足或达到上限，返回是否在限内完成
    pub async fn run(&mut self, limit: usize) -> bool {
        for _ in 0..limit {
            if self.done() {
                return true;
            }
            self.sim.tick().await.expect("update failed");
            self.ticks += 1;
            self.collect();
        }
        self.done()
    }

    pub fn received_a_to_b(&self) -> &[u8] {
        &self.got_a_to_b
    }

    pub fn received_b_to_a(&self) -> &[u8] {
        &self.got_b_to_a
    }

    /// 按消息边界取回的 A→B 消息（流模式下每次 recv 的切片）
    pub fn messages_a_to_b(&self) -> &[Vec<u8>] {
        &self.msg_a_to_b
    }

    pub fn messages_b_to_a(&self) -> &[Vec<u8>] {
        &self.msg_b_to_a
    }

    pub fn ticks(&self) -> usize {
        self.ticks
    }
}

/// A 单向发送并回收结果，返回 (是否完成, 收到的字节, 消耗 tick)
pub async fn transfer_a_to_b(
    sim: &mut Sim,
    payload: &[u8],
    limit: usize,
) -> (bool, Vec<u8>, usize) {
    sim.a.borrow_mut().send(payload).expect("send failed");
    let mut rdv = Rendezvous::new(sim, payload.len(), 0);
    let ok = rdv.run(limit).await;
    (ok, rdv.received_a_to_b().to_vec(), rdv.ticks())
}

/// 双向同时发送并回收结果
pub async fn transfer_both(
    sim: &mut Sim,
    a_payload: &[u8],
    b_payload: &[u8],
    limit: usize,
) -> (bool, Vec<u8>, Vec<u8>, usize) {
    sim.a.borrow_mut().send(a_payload).expect("send failed");
    sim.b.borrow_mut().send(b_payload).expect("send failed");
    let mut rdv = Rendezvous::new(sim, a_payload.len(), b_payload.len());
    let ok = rdv.run(limit).await;
    (ok, rdv.received_a_to_b().to_vec(), rdv.received_b_to_a().to_vec(), rdv.ticks())
}

/// 配置矩阵中的一项
pub struct Case {
    pub name: &'static str,
    pub a: KcpConfig,
    pub b: KcpConfig,
}

impl Case {
    fn same(name: &'static str, cfg: KcpConfig) -> Self {
        Self { name, b: cfg.clone(), a: cfg }
    }
}

/// 两端互连的常用配置组合
///
/// 窗口取得较小以保证消息模式下的单条消息也能整条落入接收窗口
/// （分片数 > rcv_wnd 在消息模式下必然停滞，这是协议语义而非缺陷）。
pub fn config_matrix() -> Vec<Case> {
    let stream = KcpConfig::stream_mode();
    let normal = KcpConfig {
        mtu: 700,
        interval: 20,
        nodelay: true,
        fastresend: 2,
        nocwnd: false,
        snd_wnd: 8,
        rcv_wnd: 16,
        stream_mode: false,
    };
    let tiny_mtu = KcpConfig { mtu: 128, interval: 20, ..KcpConfig::fast_mode_with_wnd(16, 64) };
    let big_wnd = KcpConfig::fast_mode_with_wnd(64, 128);
    let no_fast = KcpConfig { fastresend: 0, ..KcpConfig::fast_mode_with_wnd(16, 64) };
    let long_interval = KcpConfig { interval: 60, ..KcpConfig::fast_mode_with_wnd(16, 64) };
    let small_rcv = KcpConfig { rcv_wnd: 4, ..KcpConfig::fast_mode_with_wnd(4, 4) };

    vec![
        Case::same("stream", stream),
        Case::same("normal", normal),
        Case::same("tiny_mtu", tiny_mtu),
        Case::same("big_wnd", big_wnd),
        Case::same("no_fastresend", no_fast),
        Case::same("long_interval", long_interval),
        Case::same("small_rcv_wnd", small_rcv),
        // 非对称：一端窗口远小于另一端
        Case {
            name: "asym_wnd",
            a: KcpConfig::fast_mode_with_wnd(64, 64),
            b: KcpConfig::fast_mode_with_wnd(4, 32),
        },
        // 非对称：一端拥塞控制开启
        Case {
            name: "asym_cwnd",
            a: KcpConfig::fast_mode_with_wnd(32, 64),
            b: KcpConfig { nocwnd: false, ..KcpConfig::fast_mode_with_wnd(32, 64) },
        },
        // 非对称：MTU 不同
        Case {
            name: "asym_mtu",
            a: KcpConfig { mtu: 900, ..KcpConfig::stream_mode() },
            b: KcpConfig { mtu: 1400, ..KcpConfig::stream_mode() },
        },
    ]
}

/// 随机的网络损伤参数组合
pub fn random_net(rng: &mut super::Rng) -> Net {
    Net {
        loss: (rng.next_u64() % 30) as f64 / 100.0,
        delay: 1 + rng.next_u64() % 12,
        jitter: rng.next_u64() % 6,
        reorder: (rng.next_u64() % 20) as f64 / 100.0,
        duplicate: (rng.next_u64() % 15) as f64 / 100.0,
    }
}

/// 原始包头字段，用于绕过编码器制造畸形输入
#[derive(Clone, Copy, Default)]
pub struct RawHeader {
    pub conv: u32,
    pub cmd: u8,
    pub frg: u8,
    pub wnd: u16,
    pub ts: u64,
    pub sn: u32,
    pub una: u32,
}

impl RawHeader {
    pub fn push(conv: u32, sn: u32) -> Self {
        Self { conv, cmd: KcpCmd::Push as u8, wnd: 1024, sn, ..Self::default() }
    }
}

/// 按给定包头拼出一个完整报文
pub fn raw_packet(header: RawHeader, payload: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(IKCP_OVERHEAD + payload.len());
    raw.extend_from_slice(&header.conv.to_be_bytes());
    raw.push(header.cmd);
    raw.push(header.frg);
    raw.extend_from_slice(&header.wnd.to_be_bytes());
    raw.extend_from_slice(&header.ts.to_be_bytes());
    raw.extend_from_slice(&header.sn.to_be_bytes());
    raw.extend_from_slice(&header.una.to_be_bytes());
    raw.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    raw.extend_from_slice(payload);
    raw
}

pub fn push_packet(conv: u32, sn: u32, payload: &[u8]) -> Vec<u8> {
    raw_packet(RawHeader::push(conv, sn), payload)
}

pub fn control_packet(conv: u32, cmd: KcpCmd, wnd: u16) -> Vec<u8> {
    raw_packet(RawHeader { conv, cmd: cmd as u8, wnd, ..RawHeader::default() }, &[])
}

/// 单端实例 + 抓包槽，供输入侧用例使用
pub struct Sink {
    pub kcp: Rc<RefCell<Kcp>>,
    pub out: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl Sink {
    pub fn new(cfg: KcpConfig) -> Self {
        use std::sync::{Arc, Mutex};
        let kcp = Rc::new(RefCell::new(Kcp::new(1, cfg).expect("create kcp")));
        let out: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let sink = out.clone();
            kcp.borrow_mut().set_output(move |data| {
                let sink = sink.clone();
                Box::pin(async move {
                    sink.lock().unwrap().push(data.to_vec());
                    Ok::<usize, kcp_ovo::KcpError>(data.len())
                })
            });
        }
        Self { kcp, out }
    }

    pub async fn update(&mut self, now: u64) -> kcp_ovo::KcpResult<()> {
        self.kcp.borrow_mut().update(now).await
    }

    pub fn emitted(&self) -> Vec<Vec<u8>> {
        self.out.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.out.lock().unwrap().clear();
    }

    /// 统计指定命令字被发出的次数
    pub fn count_cmd(&self, cmd: KcpCmd) -> usize {
        self.emitted().iter().filter(|p| super::parse_cmd(p) == Some(cmd)).count()
    }
}
