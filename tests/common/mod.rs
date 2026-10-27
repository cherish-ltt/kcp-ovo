//! 黑盒测试公共设施
//!
//! 只使用 crate 的公开 API（`Kcp::new` / `set_output` / `send` / `recv` /
//! `input` / `update` / `check` / `peeksize`）搭建两点链路，不触碰任何内部字段，
//! 因此这些用例能捕捉到真实部署中会暴露的端到端缺陷。
//!
//! 时间是完全虚拟的：`Sim` 自己维护毫秒时钟，逐 tick 推进并路由在途数据包，
//! 因此测试是确定性的、与机器速度无关。

#![allow(dead_code)]
// 测试设施内部共享同一对 Kcp 实例，跨 await 持有借用是有意的：
// 每次 await 都在单线程上立即完成，不存在并发重入。
#![allow(clippy::await_holding_refcell_ref)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::generate_conv;
use kcp_ovo::{KcpConfig, KcpResult};

pub mod stress;

/// 确定性伪随机数发生器（xorshift64*），测试必须可复现，不能用系统随机源
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// [0, 1) 区间的浮点数
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn chance(&mut self, ratio: f64) -> bool {
        self.next_f64() < ratio
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        for chunk in out.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
        out
    }

    /// 生成 [1, max_segments] 段长度的随机数据
    pub fn bytes_segments(&mut self, mss: usize, max_segments: usize) -> Vec<u8> {
        let segs = 1 + self.next_u64() as usize % max_segments;
        let jitter = self.next_u64() as usize % 7;
        let len = (mss * segs).saturating_sub(jitter).max(1);
        self.bytes(len)
    }
}

/// 虚拟网络参数
#[derive(Clone)]
pub struct Net {
    /// 丢包率 [0, 1]
    pub loss: f64,
    /// 单程基础延迟（毫秒）
    pub delay: u64,
    /// 额外随机抖动上限（毫秒），0 表示无抖动
    pub jitter: u64,
    /// 触发一次乱序的概率（把同一批到达的包倒序投递）
    pub reorder: f64,
    /// 触发一次重复投递的概率
    pub duplicate: f64,
    /// 断网窗口 `(起点, 长度)`：窗口内两个方向的包全部丢弃。
    /// 用来模拟真实网络中的黑洞期（切换基站、链路闪断等）。
    pub blackout: Option<(u64, u64)>,
}

impl Default for Net {
    fn default() -> Self {
        Self { loss: 0.0, delay: 10, jitter: 0, reorder: 0.0, duplicate: 0.0, blackout: None }
    }
}

impl Net {
    pub fn loss(loss: f64) -> Self {
        Self { loss, ..Self::default() }
    }

    pub fn with_delay(mut self, delay: u64) -> Self {
        self.delay = delay;
        self
    }

    pub fn with_jitter(mut self, jitter: u64) -> Self {
        self.jitter = jitter;
        self
    }

    pub fn with_reorder(mut self, reorder: f64, duplicate: f64) -> Self {
        self.reorder = reorder;
        self.duplicate = duplicate;
        self
    }

    /// 从 `start` 毫秒起断网 `len` 毫秒
    pub fn with_blackout(mut self, start: u64, len: u64) -> Self {
        self.blackout = Some((start, len));
        self
    }

    fn in_blackout(&self, now: u64) -> bool {
        matches!(self.blackout, Some((start, len)) if now >= start && now < start + len)
    }
}

struct InFlight {
    packet: Vec<u8>,
    arrive_at: u64,
}

/// 二点链路模拟器
pub struct Sim {
    pub a: Rc<RefCell<Kcp>>,
    pub b: Rc<RefCell<Kcp>>,
    pub now: u64,
    pub net: Net,
    a_to_b: VecDeque<InFlight>,
    b_to_a: VecDeque<InFlight>,
    rng: Rng,
    // 由 output 回调写入，drive() 取出并路由
    out_a: Arc<Mutex<Vec<Vec<u8>>>>,
    out_b: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Sim {
    /// 两端同配置
    pub fn new(cfg: KcpConfig, net: Net, seed: u64) -> Self {
        Self::asymmetric(cfg.clone(), cfg, net, seed)
    }

    /// 两端不同配置（用于验证非对称窗口、模式协商等场景）
    pub fn asymmetric(cfg_a: KcpConfig, cfg_b: KcpConfig, net: Net, seed: u64) -> Self {
        let conv = generate_conv();
        Self::with_conv(conv, conv, cfg_a, cfg_b, net, seed)
    }

    /// 指定两端的 conv（验证 conv 协商与不匹配丢弃）
    #[allow(clippy::too_many_arguments)]
    pub fn with_conv(
        conv_a: u32,
        conv_b: u32,
        cfg_a: KcpConfig,
        cfg_b: KcpConfig,
        net: Net,
        seed: u64,
    ) -> Self {
        let a = Rc::new(RefCell::new(Kcp::new(conv_a, cfg_a).expect("create kcp a")));
        let b = Rc::new(RefCell::new(Kcp::new(conv_b, cfg_b).expect("create kcp b")));
        let out_a: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let out_b: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

        {
            let sink = out_a.clone();
            a.borrow_mut().set_output(move |data| {
                let sink = sink.clone();
                Box::pin(async move {
                    sink.lock().unwrap().push(data.to_vec());
                    Ok::<usize, kcp_ovo::KcpError>(data.len())
                })
            });
        }
        {
            let sink = out_b.clone();
            b.borrow_mut().set_output(move |data| {
                let sink = sink.clone();
                Box::pin(async move {
                    sink.lock().unwrap().push(data.to_vec());
                    Ok::<usize, kcp_ovo::KcpError>(data.len())
                })
            });
        }

        Self {
            a,
            b,
            now: 0,
            net,
            a_to_b: VecDeque::new(),
            b_to_a: VecDeque::new(),
            rng: Rng::new(seed),
            out_a,
            out_b,
        }
    }

    fn enqueue(&mut self, from_a: bool, batch: Vec<Vec<u8>>) {
        let loss = self.net.loss;
        let delay = self.net.delay;
        let jitter = self.net.jitter;
        let duplicate = self.net.duplicate;
        let reorder = self.net.reorder;
        let now = self.now;

        // 黑洞期内整批丢弃，且不消耗随机数，保证其它参数的复现性不受影响
        if self.net.in_blackout(now) {
            return;
        }

        let mut accepted: Vec<InFlight> = Vec::new();
        for packet in batch {
            if self.rng.chance(loss) {
                continue;
            }
            let mut arrive_at = now + delay;
            if jitter > 0 {
                arrive_at += self.rng.next_u64() % (jitter + 1);
            }
            accepted.push(InFlight { packet: packet.clone(), arrive_at });
            if duplicate > 0.0 && self.rng.chance(duplicate) {
                accepted.push(InFlight { packet, arrive_at });
            }
        }
        if reorder > 0.0 && accepted.len() > 1 && self.rng.chance(reorder) {
            accepted.reverse();
        }
        let queue = if from_a { &mut self.a_to_b } else { &mut self.b_to_a };
        queue.extend(accepted);
    }

    /// 把到期的包投递给对端，返回本次投递的包数
    fn route(&mut self) -> usize {
        let now = self.now;
        let mut delivered = 0;

        // 黑洞期内已在途的包也一并丢弃，模拟真实的链路闪断
        if self.net.in_blackout(now) {
            self.a_to_b.clear();
            self.b_to_a.clear();
            return 0;
        }

        let mut ready = Vec::new();
        let mut keep = VecDeque::new();
        for item in self.a_to_b.drain(..) {
            if item.arrive_at <= now {
                ready.push(item.packet);
            } else {
                keep.push_back(item);
            }
        }
        self.a_to_b = keep;
        for packet in ready {
            let _ = self.b.borrow_mut().input(&packet);
            delivered += 1;
        }

        let mut ready = Vec::new();
        let mut keep = VecDeque::new();
        for item in self.b_to_a.drain(..) {
            if item.arrive_at <= now {
                ready.push(item.packet);
            } else {
                keep.push_back(item);
            }
        }
        self.b_to_a = keep;
        for packet in ready {
            let _ = self.a.borrow_mut().input(&packet);
            delivered += 1;
        }

        delivered
    }

    pub fn a_to_b_len(&self) -> usize {
        self.a_to_b.len()
    }

    pub fn b_to_a_len(&self) -> usize {
        self.b_to_a.len()
    }

    /// 推进一个 tick：双端 update → 路由投递
    pub async fn tick(&mut self) -> KcpResult<()> {
        self.now += 1;
        let now = self.now;
        self.a.borrow_mut().update(now).await?;
        self.b.borrow_mut().update(now).await?;
        let batch_a = std::mem::take(&mut *self.out_a.lock().unwrap());
        let batch_b = std::mem::take(&mut *self.out_b.lock().unwrap());
        self.enqueue(true, batch_a);
        self.enqueue(false, batch_b);
        self.route();
        Ok(())
    }

    /// 推进 `ticks` 个 tick
    pub async fn run(&mut self, ticks: usize) -> KcpResult<()> {
        for _ in 0..ticks {
            self.tick().await?;
        }
        Ok(())
    }

    /// 持续推进直到两端队列都清空，或达到 `limit` 个 tick
    ///
    /// 返回实际消耗的 tick 数；耗尽上限仍未清空说明协议停滞。
    pub async fn run_until_idle(&mut self, limit: usize) -> KcpResult<usize> {
        for i in 0..limit {
            self.tick().await?;
            let idle = self.a.borrow().waitsnd() == 0
                && self.b.borrow().waitsnd() == 0
                && self.a_to_b.is_empty()
                && self.b_to_a.is_empty();
            if idle {
                return Ok(i + 1);
            }
        }
        Ok(limit)
    }

    /// 连续推进并收集 A 端收到的所有数据
    pub async fn drain_b_to_a(&mut self, ticks: usize) -> KcpResult<Vec<u8>> {
        let mut out = Vec::new();
        for _ in 0..ticks {
            self.tick().await?;
            while let Ok(chunk) = self.a.borrow_mut().recv() {
                out.extend_from_slice(&chunk);
            }
        }
        Ok(out)
    }

    /// 连续推进并收集 B 端收到的所有数据
    pub async fn drain_a_to_b(&mut self, ticks: usize) -> KcpResult<Vec<u8>> {
        let mut out = Vec::new();
        for _ in 0..ticks {
            self.tick().await?;
            while let Ok(chunk) = self.b.borrow_mut().recv() {
                out.extend_from_slice(&chunk);
            }
        }
        Ok(out)
    }

    /// 收集对端当前所有可读数据（不重复推进时钟）
    pub fn take_b(&self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.b.borrow_mut().recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    pub fn take_a(&self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.a.borrow_mut().recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// 按消息边界取走 B 端所有可读数据（每项对应一次 recv）
    pub fn b_messages(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.b.borrow_mut().recv() {
            out.push(chunk.to_vec());
        }
        out
    }

    /// 按消息边界取走 A 端所有可读数据
    pub fn a_messages(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.a.borrow_mut().recv() {
            out.push(chunk.to_vec());
        }
        out
    }
}

/// 单端（不组网）控制器，用于验证单机行为：窗口探测、死链、check() 等
pub struct Solo {
    pub kcp: Rc<RefCell<Kcp>>,
    pub out: Arc<Mutex<Vec<Vec<u8>>>>,
    pub now: u64,
}

impl Solo {
    pub fn new(cfg: KcpConfig) -> Self {
        Self::with_conv(generate_conv(), cfg)
    }

    pub fn with_conv(conv: u32, cfg: KcpConfig) -> Self {
        let kcp = Rc::new(RefCell::new(Kcp::new(conv, cfg).expect("create kcp")));
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
        Self { kcp, out, now: 0 }
    }

    /// 推进时钟并 update，返回本次 update 产生的发包数
    pub async fn step(&mut self, ms: u64) -> KcpResult<usize> {
        self.now += ms;
        self.out.lock().unwrap().clear();
        self.kcp.borrow_mut().update(self.now).await?;
        let n = self.out.lock().unwrap().len();
        Ok(n)
    }

    pub async fn update(&mut self) -> KcpResult<()> {
        self.kcp.borrow_mut().update(self.now).await
    }

    pub fn take_out(&self) -> Vec<Vec<u8>> {
        self.out.lock().unwrap().clone()
    }
}

/// 解析一个数据包的命令字，用于断言控制帧种类
pub fn parse_cmd(packet: &[u8]) -> Option<kcp_ovo::KcpCmd> {
    if packet.len() < 28 {
        return None;
    }
    kcp_ovo::KcpCmd::try_from(packet[4]).ok()
}

/// FNV-1a 校验和，用于内容一致性断言
pub fn checksum(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// 断言两段字节完全相同，失败时打印首个差异位置而非整个缓冲区
pub fn assert_same(actual: &[u8], expected: &[u8]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "长度不一致: 实际 {} 字节, 期望 {} 字节",
        actual.len(),
        expected.len()
    );
    if let Some(pos) = actual.iter().zip(expected).position(|(a, b)| a != b) {
        panic!(
            "第 {} 字节起内容不一致: 实际 0x{:02x}, 期望 0x{:02x}",
            pos, actual[pos], expected[pos]
        );
    }
}
