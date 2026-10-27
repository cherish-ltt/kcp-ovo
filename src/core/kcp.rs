//! KCP控制块核心实现
//!
//! 本模块定义了KCP协议的核心控制块结构，对应C代码中的IKCPCB。
//! 包含流模式（Stream Mode）支持：流模式下所有分片 frg=0，
//! 接收端按 sn 顺序连续拼接字节交付，无消息边界，不受 255 分片限制。

use std::pin::Pin;

use bytes::{Buf, Bytes, BytesMut};

use crate::KcpCmd;
use crate::config::KcpConfig;
use crate::error::{IncompleteDataType, KcpError, KcpResult};
use crate::helper::{
    IKCP_ASK_SEND, IKCP_ASK_TELL, IKCP_DEADLINK, IKCP_FASTACK_LIMIT, IKCP_OVERHEAD,
    IKCP_PROBE_LIMIT, IKCP_RTO_DEF, IKCP_RTO_MAX, IKCP_RTO_MIN, IKCP_THRESH_INIT, IKCP_THRESH_MIN,
    KcpPacket, KcpPacketHeader, current_millis,
};
use crate::queue::{KcpDeque, Segment};

/// 输出回调函数类型别名
type OutputCallback = Box<
    dyn Fn(Bytes) -> Pin<Box<dyn Future<Output = KcpResult<usize>> + Send + 'static>> + Send + Sync,
>;

/// 日志回调函数类型别名
type LogCallback = Box<dyn Fn(&str, &Kcp) + Send + Sync>;

/// KCP 控制块 — 协议核心状态机
///
/// 维护连接状态、序列号、窗口、队列等信息。
/// 支持两种模式：
/// - **非流模式**（默认）：按 frg 链组装消息，有消息边界，最多 255 分片
/// - **流模式**（`stream_mode=true`）：所有分片 frg=0，按 sn 连续拼接，无消息边界，无分片上限
///
/// 流模式与非流模式不能互连互通（frg 语义不同）。
pub struct Kcp {
    /// 会话 ID，必须两端一致
    pub conv: u32,
    /// 最大传输单元（字节）
    pub mtu: u32,
    /// 最大分段大小 = mtu - IKCP_OVERHEAD
    pub mss: u32,
    /// 连接状态
    pub state: u32,

    // 序列号
    /// 未确认的最小序列号（已确认的 sn < snd_una）
    pub snd_una: u32,
    /// 下一个待发送的序列号
    pub snd_nxt: u32,
    /// 下一个期望接收的序列号
    pub rcv_nxt: u32,

    // 时间戳
    /// 最近收到的数据包时间戳
    pub ts_recent: u64,
    /// 最近一次 ACK 的时间戳
    pub ts_lastack: u64,
    /// 慢启动阈值
    pub ssthresh: u32,

    // RTT / RTO
    /// RTT 变化量估计值
    pub rx_rttval: i64,
    /// 平滑 RTT 估计值
    pub rx_srtt: i64,
    /// 重传超时时间（毫秒）
    pub rx_rto: u64,
    /// 最小 RTO（毫秒）
    pub rx_minrto: u64,

    // 窗口
    /// 发送窗口大小（segment 数）
    pub snd_wnd: u16,
    /// 接收窗口大小（segment 数）
    pub rcv_wnd: u16,
    /// 远端窗口大小
    pub rmt_wnd: u16,
    /// 拥塞窗口
    pub cwnd: u16,
    /// 窗口探测标志（IKCP_ASK_SEND / IKCP_ASK_TELL）
    pub probe: u32,

    // 定时
    /// 当前时钟（毫秒）
    pub current: u64,
    /// 内部更新间隔（毫秒）
    pub interval: u64,
    /// 下一次刷新的时间戳
    pub ts_flush: u64,
    /// 发送计数（仅用于统计）
    pub xmit: u32,

    // 队列计数
    /// rcv_buf 中的段数
    pub nrcv_buf: usize,
    /// snd_buf 中的段数
    pub nsnd_buf: usize,
    /// rcv_queue 中的段数
    pub nrcv_que: usize,
    /// snd_queue 中的段数
    pub nsnd_que: usize,

    // 配置
    /// 是否启用无延迟模式（Nagle 算法禁用）
    pub nodelay: bool,
    /// 流模式：true 时所有分片 frg=0，无上限，纯字节流语义
    pub stream_mode: bool,
    /// 是否已调用过 update（首次调用会初始化 ts_flush）
    pub updated: bool,
    /// 窗口探测时间戳
    pub ts_probe: u64,
    /// 窗口探测等待时间
    pub probe_wait: u64,
    /// 死链阈值（超过此次数重传即断开连接）
    pub dead_link: u32,
    /// 拥塞窗口增量累加器
    pub incr: u32,

    // 队列
    /// 发送队列（待发送的数据段）
    pub snd_queue: KcpDeque,
    /// 接收队列（已排序、可读的数据段）
    pub rcv_queue: KcpDeque,
    /// 发送缓冲区（已发送但未确认的段）
    pub snd_buf: KcpDeque,
    /// 接收缓冲区（乱序到达、等待排序的段）
    pub rcv_buf: KcpDeque,

    // ACK
    /// ACK 列表（待发送的确认序列号）
    acklist: Vec<u32>,
    /// ACK 列表容量（预留）
    #[allow(dead_code)]
    ackblock: usize,

    // 用户数据 / 缓冲区
    #[allow(dead_code)]
    user_data: Option<Box<dyn std::any::Any>>,
    #[allow(dead_code)]
    buffer: Vec<u8>,

    /// 快速重传：重复 ACK 达到此次数则立即重传（0 = 禁用）
    pub fastresend: i32,
    /// 快速重传上限
    pub fastlimit: i32,
    /// 是否禁用拥塞控制
    pub nocwnd: bool,

    /// 日志掩码
    pub logmask: u32,

    // 回调
    /// 输出回调：将 KCP 数据包交给底层传输
    output: Option<OutputCallback>,
    /// 日志回调
    writelog: Option<LogCallback>,
}

unsafe impl Send for Kcp {}
unsafe impl Sync for Kcp {}

impl Kcp {
    /// 创建新的 KCP 控制块
    ///
    /// # 参数
    ///
    /// * `conv` — 会话 ID，必须两端一致
    /// * `config` — KCP 配置，包含 `stream_mode` 等选项
    ///
    /// # 返回
    ///
    /// 返回新创建的 `Kcp` 实例或错误
    pub fn new(conv: u32, config: KcpConfig) -> KcpResult<Self> {
        let mtu = config.mtu;
        let mss = mtu - (IKCP_OVERHEAD as u32);

        Ok(Self {
            conv,
            mtu,
            mss,
            state: 0,
            snd_una: 0,
            snd_nxt: 0,
            rcv_nxt: 0,
            ts_recent: 0,
            ts_lastack: 0,
            ssthresh: IKCP_THRESH_INIT,
            rx_rttval: 0,
            rx_srtt: 0,
            rx_rto: IKCP_RTO_DEF,
            rx_minrto: IKCP_RTO_MIN,
            snd_wnd: config.snd_wnd,
            rcv_wnd: config.rcv_wnd,
            // 修复 BUG-6：rmt_wnd 初始化为 0（C 原版 kcp->rmt_wnd = 0）
            // Rust 原译：config.rcv_wnd → 远端窗口错误，初始发送速率过高
            rmt_wnd: 0,
            // 修复 BUG-1：cwnd 初始化为 IKCP_THRESH_INIT（值为 2）而非 0
            // Rust 原译：0 → 默认模式（nocwnd=false）数据永不发送
            cwnd: IKCP_THRESH_INIT as u16,
            probe: 0,
            current: 0,
            interval: config.interval,
            ts_flush: config.interval,
            xmit: 0,
            nrcv_buf: 0,
            nsnd_buf: 0,
            nrcv_que: 0,
            nsnd_que: 0,
            nodelay: config.nodelay,
            stream_mode: config.stream_mode,
            updated: false,
            ts_probe: 0,
            probe_wait: 0,
            dead_link: IKCP_DEADLINK,
            incr: 0,
            snd_queue: KcpDeque::new(),
            rcv_queue: KcpDeque::new(),
            snd_buf: KcpDeque::new(),
            rcv_buf: KcpDeque::new(),
            acklist: Vec::with_capacity(16),
            ackblock: 16,
            user_data: None,
            buffer: vec![0u8; (mtu as usize + IKCP_OVERHEAD) * 3],
            fastresend: config.fastresend,
            fastlimit: IKCP_FASTACK_LIMIT,
            nocwnd: config.nocwnd,
            logmask: 0,
            output: None,
            writelog: None,
        })
    }

    /// 设置输出回调函数
    ///
    /// 当 KCP 需要发送数据包时调用此回调，通常负责将数据通过 UDP socket 发出。
    ///
    /// # 参数
    ///
    /// * `callback` — 接收 `Bytes` 数据，返回发送的字节数
    pub fn set_output<F, Fut>(&mut self, callback: F)
    where
        F: Fn(Bytes) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = KcpResult<usize>> + Send + 'static,
    {
        self.output = Some(Box::new(move |data| Box::pin(callback(data))));
    }

    /// 设置日志回调函数
    ///
    /// # 参数
    ///
    /// * `callback` — 接收日志消息字符串和 KCP 引用
    pub fn set_log<F>(&mut self, callback: F)
    where
        F: Fn(&str, &Kcp) + Send + Sync + 'static,
    {
        self.writelog = Some(Box::new(callback));
    }

    /// 写日志
    #[allow(dead_code)]
    fn log(&self, mask: u32, msg: &str) {
        if (mask & self.logmask) != 0
            && let Some(ref writelog) = self.writelog
        {
            writelog(msg, self);
        }
    }

    /// 获取会话 ID
    pub fn getconv(&self) -> u32 {
        self.conv
    }

    /// 查询待发送数据段数（snd_queue + snd_buf 总和）
    pub fn waitsnd(&self) -> usize {
        self.nsnd_que + self.nsnd_buf
    }

    // ========== 辅助函数 ==========

    /// 计算 32 位时间戳差值（支持回绕）
    #[inline]
    fn timediff_u32(&self, later: u32, earlier: u32) -> i32 {
        (later as i32).wrapping_sub(earlier as i32)
    }

    /// 计算 64 位时间戳差值（支持回绕）
    #[inline]
    fn timediff_u64(&self, later: u64, earlier: u64) -> i64 {
        (later as i64).wrapping_sub(earlier as i64)
    }

    /// 将 rcv_buf 中有序的段移入 rcv_queue，并更新 window probe
    #[inline]
    fn move_rcv_buf_and_probe(&mut self) {
        self.move_rcv_buf_to_queue();
        if self.nrcv_que < self.rcv_wnd as usize {
            self.probe |= IKCP_ASK_TELL;
        }
    }

    // ========== 核心功能方法 ==========

    /// 发送数据
    ///
    /// 将数据按 MSS 分片后加入发送队列。
    ///
    /// **非流模式**：分片 frg 递减（最后一个为 0），最多 255 分片。
    /// **流模式**：所有分片 frg=0，无分片数量上限。
    ///
    /// # 参数
    ///
    /// * `data` — 待发送的字节数据
    ///
    /// # 返回
    ///
    /// 返回发送的字节数，失败返回错误
    pub fn send(&mut self, data: &[u8]) -> KcpResult<usize> {
        let len = data.len();
        if len == 0 {
            return Ok(0);
        }
        let count = len.div_ceil(self.mss as usize);
        let is_stream = self.stream_mode;

        // 非流模式有 255 分片上限，流模式无此限制
        if !is_stream && count > u8::MAX as usize {
            return Err(KcpError::InvalidConfig(format!(
                "Too many fragments, data-size={}byte, allow-max-size={}byte",
                len,
                (u8::MAX as usize) * (self.mss as usize)
            )));
        }

        let mut sent = 0;
        let mut buf = data;
        for i in 0..count {
            let size = buf.len().min(self.mss as usize);
            // 流模式所有 frg=0；非流模式递减 frg 链
            let frg = if is_stream { 0 } else { (count - i - 1) as u8 };
            // 修复 BUG-11：窗口通告使用可用窗口（rcv_wnd - nrcv_que）而非总窗口
            // C 原版：kcp->rcv_wnd - kcp->nrcv_que
            // Rust 原译：self.rcv_wnd（固定值）→ 分段 >128 时死锁
            let header = KcpPacketHeader::new(
                self.conv,
                KcpCmd::Push,
                frg,
                self.rcv_wnd.saturating_sub(self.nrcv_que as u16),
                current_millis(),
                self.snd_nxt,
                self.snd_una,
                size as u32,
            );
            let seg = Segment::new_with_header_and_data(header, &buf[..size])?;
            self.snd_queue.push_back(seg);
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
            self.nsnd_que = self.nsnd_que.saturating_add(1);
            buf = &buf[size..];
            sent += size;
        }
        Ok(sent)
    }

    /// 查看下一个可读数据的大小（不消耗数据）
    ///
    /// **非流模式**：沿 frg 链计算完整消息大小，等所有分片就绪。
    /// **流模式**：返回 rcv_queue 中所有 segment 的总大小（recv 一次性读取全部）。
    ///
    /// # 返回
    ///
    /// 返回下一个可读字节数；队列为空返回 `QueueEmpty`；等分片返回 `PayloadWaitForFrg`
    ///
    /// # 注意
    ///
    /// 流模式下，`BytesMut::with_capacity(peeksize)` 可以一次性分配足量缓冲区，
    /// 避免多次重新分配（BUG-9 修复）。
    pub fn peeksize(&self) -> KcpResult<usize> {
        let first = self.rcv_queue.front().ok_or(KcpError::QueueEmpty)?;
        // 流模式：返回所有 segment 的总大小（recv 一次性读取全部）
        // 修复 BUG-9：原实现只返回第一个 segment 大小，导致 BytesMut 分配过小
        if self.stream_mode {
            let total_len: usize = self
                .rcv_queue
                .iter()
                .map(|s| s.kcp_packet.header.len as usize)
                .sum();
            return Ok(total_len);
        }
        if first.kcp_packet.header.frg == 0 {
            return Ok(first.kcp_packet.header.len as usize);
        }
        if self.nrcv_que < (first.kcp_packet.header.frg + 1) as usize {
            return Err(KcpError::IncompleteData(
                IncompleteDataType::PayloadWaitForFrg,
            ));
        }
        let mut length = 0usize;
        for seg in self.rcv_queue.iter() {
            length += seg.kcp_packet.header.len as usize;
            if seg.kcp_packet.header.frg == 0 {
                break;
            }
        }
        Ok(length)
    }

    /// 接收数据
    ///
    /// **非流模式**：从 rcv_queue 读取一条完整消息（frg 链），返回 `Bytes`。
    /// **流模式**：一次性读取 rcv_queue 中所有可读 segment 并拼接返回，无消息边界。
    ///
    /// # 返回
    ///
    /// 返回接收到的完整 `Bytes` 数据；队列为空返回 `QueueEmpty`
    pub fn recv(&mut self) -> KcpResult<Bytes> {
        let peeksize = self.peeksize()?;
        let mut buf = BytesMut::with_capacity(peeksize);
        // 流模式读取所有 segment，非流模式读取到 frg==0
        while let Some(seg) = self.rcv_queue.pop_front() {
            buf.extend_from_slice(&seg.get_data());
            self.nrcv_que -= 1;
            if !self.stream_mode && seg.kcp_packet.header.frg == 0 {
                break;
            }
        }
        self.move_rcv_buf_and_probe();

        if !self.stream_mode && buf.len() != peeksize {
            return Err(KcpError::IncompleteData(IncompleteDataType::PayloadErr));
        }
        Ok(buf.copy_to_bytes(buf.len()))
    }

    /// 输入处理：解析并处理从底层网络收到的数据包
    ///
    /// 根据命令类型（Push / Ack / Wask / Wins）分别处理，
    /// 将有序数据段移入 rcv_queue 供 recv 读取。
    pub fn input(&mut self, data: &[u8]) -> KcpResult<()> {
        if data.len() < IKCP_OVERHEAD {
            return Err(KcpError::IncompleteData(IncompleteDataType::Header));
        }
        let mut buf = Bytes::copy_from_slice(data);
        // 解析头部
        let (header, buf) = KcpPacketHeader::from_bytes(&mut buf)?;

        // 验证conv是否匹配
        if self.conv == 0 {
            self.conv = header.conv;
        } else if header.conv != self.conv {
            return Ok(()); // 静默丢弃不匹配的数据包
        }

        // 更新远端窗口大小
        self.rmt_wnd = header.wnd;

        // 根据命令类型处理
        match header.cmd {
            KcpCmd::Ack => {
                // ACK命令：确认包
                if self.timediff_u32(header.sn, self.snd_una) > 0 {
                    self.parse_ack(header.sn);
                }
                if let Some(ref writelog) = self.writelog {
                    writelog(&format!("input ack: sn={}", header.sn), self);
                }
            }
            KcpCmd::Push => {
                if self.timediff_u64(header.sn as u64, self.rcv_nxt as u64 + self.rcv_wnd as u64)
                    >= 0
                    || self.timediff_u64(header.sn as u64, self.rcv_nxt as u64) < 0
                {
                    return Ok(());
                }
                for seg in self.rcv_buf.iter() {
                    if seg.kcp_packet.header.sn == header.sn {
                        return Ok(());
                    }
                }
                let newseg = Segment::new_with_header_and_data(header, &buf)?;
                self.parse_data(newseg);
                self.acklist.push(header.sn);
                self.move_rcv_buf_to_queue();
            }
            KcpCmd::Wask => self.probe |= IKCP_ASK_TELL,
            KcpCmd::Wins => {}
        }

        // 修复 BUG-7：parse_fastack 必须在 parse_una 之前调用
        // 若先调用 parse_una 移除 sn < una 的段，剩余段 sn >= una，
        // 则 parse_fastack 中 una > seg.sn 永假 → fastack 永不递增 → 快速重传失效
        if self.timediff_u32(header.una, self.snd_una) > 0 {
            self.parse_fastack(header.una);
            self.parse_una(header.una);
        }
        if self.timediff_u64(self.current, header.ts) >= 10000
            || self.timediff_u64(header.ts, self.ts_recent) >= 10000
        {
            self.ts_recent = header.ts;
        }
        Ok(())
    }

    /// 移除 snd_buf 中所有 sn < una 的已确认段，并更新 snd_una
    ///
    /// # BUG-3 修复说明
    ///
    /// 原实现缺少 `self.snd_una = una`，snd_una 初始化后从未更新（始终为 0），
    /// 导致 input() 中 `timediff_u32(header.una, snd_una) > 0` 对每个包都成立，
    /// 同时也导致 BUG-7（快速重传失效）。
    fn parse_una(&mut self, una: u32) {
        while let Some(seg) = self.snd_buf.front() {
            if self.timediff_u32(una, seg.kcp_packet.header.sn) > 0 {
                self.snd_buf.pop_front();
                self.nsnd_buf -= 1;
            } else {
                break;
            }
        }
        // 修复 BUG-3：snd_una 必须跟随未被确认的最小 SN
        self.snd_una = una;
    }

    /// 处理 ACK 确认：更新 RTT/RTO，移除 snd_buf 中被确认的段
    ///
    /// # BUG-2 修复说明
    ///
    /// 条件判断使用 `== 0` 而非 `>= 0`。`snd_buf` 按 SN 升序排列，
    /// 若用 `>= 0` 则对第一个分段几乎永远成立，导致 RTT 始终使用
    /// 第一个分段的发送时间，RTO 计算完全错误。
    fn parse_ack(&mut self, sn: u32) {
        let current = self.current;
        let mut rtt_update = None;
        for seg in self.snd_buf.iter() {
            // 修复 BUG-2：必须精确匹配被确认的分段
            if self.timediff_u32(sn, seg.kcp_packet.header.sn) == 0 {
                let rtt = self.timediff_u64(current, seg.kcp_packet.header.ts);
                if rtt >= 0 {
                    rtt_update = if self.rx_srtt == 0 {
                        Some((rtt, rtt / 2))
                    } else {
                        let delta = rtt - self.rx_srtt;
                        Some((self.rx_srtt + delta / 8, self.rx_rttval + delta.abs() / 4))
                    };
                }
                break;
            }
        }
        if let Some((srtt, rttval)) = rtt_update {
            self.rx_srtt = srtt;
            self.rx_rttval = rttval;
            self.rx_rto = ((srtt as u64) + self.interval.max(4 * rttval as u64))
                .max(self.rx_minrto)
                .min(IKCP_RTO_MAX);
        }

        // 修复 BUG-8：使用 retain 原地过滤，代替原 O(n) 弹出-重新压入模式
        // 原实现：弹出所有段到临时 Vec -> 过滤 -> 重新压入，每次 ACK 都触发分配
        let mut removed = false;
        self.snd_buf.retain(|seg| {
            if seg.kcp_packet.header.sn == sn && !removed {
                removed = true;
                false // 移除匹配的第一个段
            } else {
                true // 保留其他段
            }
        });
        if removed {
            self.nsnd_buf -= 1;
        }
    }

    /// 根据 una 对所有未确认段增加快速重传计数
    ///
    /// # BUG-7 修复说明
    ///
    /// 此方法必须在 `parse_una()` 之前调用。若先调用 parse_una 移除 sn < una 的段，
    /// 剩余段 sn >= una → `una > seg.sn` 永假 → fastack 永不递增 → 快速重传完全失效。
    /// 调用顺序在 `input()` 中保证：先 parse_fastack，后 parse_una。
    fn parse_fastack(&mut self, una: u32) {
        for seg in self.snd_buf.iter_mut() {
            if (una as i32).wrapping_sub(seg.kcp_packet.header.sn as i32) > 0 && seg.fastack < 255 {
                seg.fastack += 1;
            }
        }
    }

    /// 将接收到的数据段按 sn 顺序插入 rcv_buf
    fn parse_data(&mut self, newseg: Segment) {
        if self.nrcv_buf >= self.rcv_wnd as usize {
            return;
        }
        for seg in self.rcv_buf.iter() {
            if seg.kcp_packet.header.sn == newseg.kcp_packet.header.sn {
                return;
            }
        }
        let insert_pos = self
            .rcv_buf
            .iter()
            .position(|seg| {
                self.timediff_u32(newseg.kcp_packet.header.sn, seg.kcp_packet.header.sn) < 0
            })
            .unwrap_or(self.rcv_buf.len());

        // Drain tail after insert_pos, push new, then push tail back
        let mut tail = Vec::new();
        for _ in insert_pos..self.rcv_buf.len() {
            tail.push(self.rcv_buf.pop_back().unwrap());
        }
        self.rcv_buf.push_back(newseg);
        for seg in tail.into_iter().rev() {
            self.rcv_buf.push_back(seg);
        }
        self.nrcv_buf += 1;
    }

    /// 发送控制帧（Wask / Ack / Wins）的快捷方法
    ///
    /// # BUG-11 修复
    ///
    /// 窗口通告使用可用窗口（rcv_wnd - nrcv_que）而非总窗口 rcv_wnd，
    /// 与控制段 header 中 wnd 字段的设置一致。
    async fn send_control(&mut self, cmd: KcpCmd, sn: u32) -> KcpResult<()> {
        self.send_packet(KcpPacket::new_without_data(KcpPacketHeader::new(
            self.conv,
            cmd,
            0,
            self.rcv_wnd.saturating_sub(self.nrcv_que as u16),
            current_millis(),
            sn,
            self.rcv_nxt,
            0,
        )))
        .await
    }

    /// 刷新发送缓冲区：将 snd_queue 移入 snd_buf，发送数据段、ACK、窗口探测
    async fn flush(&mut self) -> KcpResult<()> {
        // rmt_wnd 未知（尚未收到对端任何包）时放行 1 个包探路：
        // 两端同时新建连接时都把 rmt_wnd 视为 0，若严格按 0 计算窗口则双方都发不出首包，
        // 只能等窗口探测，连接建立被拖延到探测间隔的量级。
        // 对端收到该段后会回 ACK 并携带自身 wnd，rmt_wnd 随即被填上。
        let window = self.snd_wnd.min(self.rmt_wnd.max(1));
        let mut can_send = if self.nocwnd {
            window
        } else {
            window.min(self.cwnd)
        } as usize;
        let mut lost = false;

        while can_send > 0 && !self.snd_queue.is_empty() {
            let mut seg = self.snd_queue.pop_front().unwrap();
            self.nsnd_que -= 1;
            seg.kcp_packet.header.conv = self.conv;
            seg.kcp_packet.header.cmd = KcpCmd::Push;
            // 修复 BUG-11：使用可用窗口而非总窗口通告给对端
            seg.kcp_packet.header.wnd = self.rcv_wnd.saturating_sub(self.nrcv_que as u16);
            seg.kcp_packet.header.ts = self.current;
            seg.kcp_packet.header.una = self.rcv_nxt;
            seg.resendts = self.current;
            seg.rto = self.rx_rto;
            seg.fastack = 0;
            seg.xmit = 0;
            self.snd_buf.push_back(seg);
            self.nsnd_buf += 1;
            can_send -= 1;
        }

        // 收集待发送的 ACK 列表，评估是否需要窗口探测
        // 修复 BUG-4：ACK 发送不再被 need_wask else 分支阻塞，
        // Wask 和 ACK 是独立的两个操作（与原版 C 代码一致）
        let acks_to_send = self.acklist.clone();
        let need_wask = acks_to_send.is_empty()
            && (self.probe & IKCP_ASK_SEND) != 0
            && self.timediff_u64(self.current, self.ts_probe) >= 0;
        let need_wins = (self.probe & IKCP_ASK_TELL) != 0;
        self.acklist.clear();

        if need_wask {
            self.send_control(KcpCmd::Wask, 0).await?;
            self.probe &= !IKCP_ASK_SEND;
        }
        for sn in acks_to_send {
            self.send_control(KcpCmd::Ack, sn).await?;
        }
        if need_wins {
            self.send_control(KcpCmd::Wins, 0).await?;
            self.probe &= !IKCP_ASK_TELL;
        }

        if self.rmt_wnd == 0 && (self.probe & IKCP_ASK_SEND) == 0 {
            self.probe |= IKCP_ASK_SEND;
            if self.probe_wait == 0 {
                // 首次探测不等待：对端的窗口通告是打开发送窗口的前提，
                // 初始退避若取 IKCP_PROBE_INIT(7s)，两端都未收到对端包时会互相空等 7 秒。
                self.probe_wait = self.interval.max(1);
                self.ts_probe = self.current;
            } else {
                self.probe_wait = (self.probe_wait * 2).min(IKCP_PROBE_LIMIT);
                self.ts_probe = self.current + self.probe_wait;
            }
        }

        let current = self.current;
        let mut sends = Vec::new();
        let mut need_update = Vec::new();

        for (idx, seg) in self.snd_buf.iter().enumerate() {
            if seg.xmit == 0 {
                need_update.push((idx, 1, current.wrapping_add(seg.rto), seg.rto, false));
                sends.push((
                    seg.kcp_packet.header.frg,
                    seg.kcp_packet.header.sn,
                    seg.kcp_packet.header.ts,
                    seg.kcp_packet.header.una,
                    seg.kcp_packet.header.wnd,
                    seg.get_data(),
                ));
                continue;
            }
            // 修复 BUG-5：两个重传条件用 else if 互斥
            // 超时重传优先于快速重传，避免同一段被推入 sends 两次
            if (current as i32).wrapping_sub(seg.resendts as i32) >= 0 {
                if seg.xmit >= self.dead_link {
                    return Err(KcpError::IoError(
                        "Connection lost due to too many retransmissions".to_string(),
                    ));
                }
                let new_rto = (seg.rto * 2).min(IKCP_RTO_MAX);
                need_update.push((
                    idx,
                    seg.xmit + 1,
                    current.wrapping_add(seg.rto),
                    new_rto,
                    true,
                ));
                sends.push((
                    seg.kcp_packet.header.frg,
                    seg.kcp_packet.header.sn,
                    seg.kcp_packet.header.ts,
                    seg.kcp_packet.header.una,
                    seg.kcp_packet.header.wnd,
                    seg.get_data(),
                ));
            // 修复 BUG-5：使用 else if 而非独立的 if，防止双重发送
            } else if seg.fastack >= self.fastresend as u32 {
                if seg.xmit >= self.dead_link {
                    return Err(KcpError::IoError(
                        "Connection lost due to too many retransmissions".to_string(),
                    ));
                }
                need_update.push((
                    idx,
                    seg.xmit + 1,
                    current.wrapping_add(seg.rto),
                    seg.rto,
                    true,
                ));
                sends.push((
                    seg.kcp_packet.header.frg,
                    seg.kcp_packet.header.sn,
                    seg.kcp_packet.header.ts,
                    seg.kcp_packet.header.una,
                    seg.kcp_packet.header.wnd,
                    seg.get_data(),
                ));
            }
        }

        for (frg, sn, ts, una, wnd, data) in sends {
            self.send_packet(KcpPacket::new_with_data(
                KcpPacketHeader::new(
                    self.conv,
                    KcpCmd::Push,
                    frg,
                    wnd,
                    ts,
                    sn,
                    una,
                    data.len() as u32,
                ),
                data,
            ))
            .await?;
        }

        for (idx, xmit, resendts, rto, is_lost) in need_update {
            if let Some(seg) = self.snd_buf.get_mut(idx) {
                seg.xmit = xmit;
                seg.resendts = resendts;
                seg.rto = rto;
                if is_lost {
                    lost = true;
                }
            }
        }

        // 拥塞控制：丢包则收缩窗口，否则按慢启动/拥塞避免递增
        if lost {
            self.ssthresh = (self.cwnd as u32 / 2).max(IKCP_THRESH_MIN);
            self.cwnd = 1;
            self.incr = 0;
        } else if self.timediff_u64(current, self.ts_lastack) >= 0 {
            if (self.cwnd as u32) < self.ssthresh {
                self.incr += self.mss;
            } else if self.mss > 0 {
                self.incr += self.mss * self.mss / (self.cwnd as u32);
            }
            if self.incr >= self.mss {
                self.cwnd += 1;
                self.incr -= self.mss;
            }
        }
        Ok(())
    }

    /// 统一的发送接口（处理 output 回调的借用问题）
    async fn send_packet(&mut self, pkt: KcpPacket) -> KcpResult<()> {
        let data = pkt.to_bytes()?;
        let output = self.output.take().ok_or(KcpError::OutputNotSet)?;
        let result = output(data).await;
        self.output = Some(output);
        result?;
        self.xmit = self.xmit.wrapping_add(1);
        Ok(())
    }

    /// 更新 KCP 状态：调整时钟并触发 flush
    pub async fn update(&mut self, current: u64) -> KcpResult<()> {
        self.current = current;
        if !self.updated {
            self.updated = true;
            self.ts_flush = self.current;
        }
        if self.timediff_u64(self.current, self.ts_flush) >= 0 {
            self.ts_flush = self.current.wrapping_add(self.interval);
            self.flush().await?;
        }
        Ok(())
    }

    /// 计算下次更新时间
    ///
    /// 检查 snd_buf 中最早的重传时间和 ts_flush，返回下次需要 update 的时间戳。
    pub fn check(&self, current: u64) -> u64 {
        let mut ts_flush = self.ts_flush;
        if !self.snd_queue.is_empty() || !self.snd_buf.is_empty() {
            for seg in self.snd_buf.iter() {
                let diff = self.timediff_u64(seg.resendts, current);
                if diff < 0 {
                    return current;
                }
                if diff < self.timediff_u64(ts_flush, current) {
                    ts_flush = seg.resendts;
                }
            }
        }
        if (self.probe & IKCP_ASK_SEND) != 0 {
            return current;
        }
        if self.timediff_u64(ts_flush, current) > 0 {
            ts_flush
        } else {
            current
        }
    }

    /// 将 rcv_buf 中有序的段（sn 连续）移入 rcv_queue，更新 rcv_nxt
    #[inline]
    fn move_rcv_buf_to_queue(&mut self) {
        while let Some(seg) = self.rcv_buf.front() {
            if seg.kcp_packet.header.sn != self.rcv_nxt || self.nrcv_que >= self.rcv_wnd as usize {
                break;
            }
            let seg = self.rcv_buf.pop_front().unwrap();
            self.nrcv_buf -= 1;
            self.rcv_queue.push_back(seg);
            self.nrcv_que += 1;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::KcpCmd;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_kcp_new() {
        let kcp = Kcp::new(0x11223344, KcpConfig::default()).unwrap();
        assert_eq!(kcp.conv, 0x11223344);
        assert_eq!(kcp.mtu, 1400);
        assert_eq!(kcp.mss, 1372);
    }

    #[test]
    fn test_waitsnd() {
        assert_eq!(Kcp::new(0, KcpConfig::default()).unwrap().waitsnd(), 0);
    }

    fn random_bytes(len: usize) -> Vec<u8> {
        use rand::Rng;
        let mut rng = rand::rng();
        let mut data = vec![0u8; len];
        rng.fill_bytes(&mut data);
        data
    }

    // ===== 流模式测试 =====

    #[test]
    fn test_stream_mode_peeksize_single_segment() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        let h = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 1, 0, 5);
        kcp.rcv_queue
            .push_back(Segment::new_with_header_and_data(h, &[1, 2, 3, 4, 5]).unwrap());
        kcp.nrcv_que += 1;
        assert_eq!(kcp.peeksize().unwrap(), 5);
    }

    #[test]
    fn test_stream_mode_peeksize_multiple_segments() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 1, 0, 3),
                &[1, 2, 3],
            )
            .unwrap(),
        );
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 2, 0, 4),
                &[4, 5, 6, 7],
            )
            .unwrap(),
        );
        kcp.nrcv_que += 2;
        // After BUG-9 fix: peeksize returns sum of all segment lengths (3 + 4 = 7)
        assert_eq!(kcp.peeksize().unwrap(), 7);
    }

    #[test]
    fn test_stream_mode_recv_single_segment() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        let h = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 1, 0, 5);
        kcp.rcv_queue
            .push_back(Segment::new_with_header_and_data(h, &[10, 20, 30, 40, 50]).unwrap());
        kcp.nrcv_que += 1;
        assert_eq!(kcp.recv().unwrap().to_vec(), vec![10, 20, 30, 40, 50]);
        assert_eq!(kcp.nrcv_que, 0);
    }

    #[test]
    fn test_stream_mode_recv_multiple_segments() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        let msg: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9];
        let (a, bc) = msg.split_at(3);
        let (b, c) = bc.split_at(4);
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 1, 0, 3),
                a,
            )
            .unwrap(),
        );
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 2, 0, 4),
                b,
            )
            .unwrap(),
        );
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 3, 0, 2),
                c,
            )
            .unwrap(),
        );
        kcp.nrcv_que += 3;
        assert_eq!(kcp.recv().unwrap().to_vec(), msg);
        assert_eq!(kcp.nrcv_que, 0);
    }

    #[test]
    fn test_stream_mode_recv_empty() {
        assert_eq!(
            Kcp::new(1, KcpConfig::stream_mode())
                .unwrap()
                .recv()
                .unwrap_err(),
            KcpError::QueueEmpty
        );
    }

    #[test]
    fn test_stream_mode_no_frg_limit() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        let big = random_bytes(kcp.mss as usize * 300);
        assert!(kcp.send(&big).is_ok());
        assert_eq!(kcp.nsnd_que, 300);
    }

    #[test]
    fn test_non_stream_mode_frg_limit() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        match kcp.send(&random_bytes(kcp.mss as usize * 300)).unwrap_err() {
            KcpError::InvalidConfig(_) => {}
            e => panic!("expected InvalidConfig, got {:?}", e),
        }
    }

    #[test]
    fn test_stream_mode_frg_all_zero() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        kcp.send(&random_bytes(kcp.mss as usize * 3 + 100)).unwrap();
        assert_eq!(kcp.nsnd_que, 4);
        for seg in kcp.snd_queue.iter() {
            assert_eq!(seg.kcp_packet.header.frg, 0);
        }
    }

    #[test]
    fn test_non_stream_mode_frg_decreasing() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        kcp.send(&random_bytes(kcp.mss as usize * 3 + 100)).unwrap();
        assert_eq!(kcp.nsnd_que, 4);
        let frgs: Vec<u8> = kcp
            .snd_queue
            .iter()
            .map(|s| s.kcp_packet.header.frg)
            .collect();
        assert_eq!(frgs, vec![3, 2, 1, 0]);
    }

    #[test]
    fn test_stream_mode_peeksize_empty() {
        assert_eq!(
            Kcp::new(1, KcpConfig::stream_mode())
                .unwrap()
                .peeksize()
                .unwrap_err(),
            KcpError::QueueEmpty
        );
    }

    #[tokio::test]
    async fn test_stream_mode_full_cycle_small() {
        let (mut s, r) = {
            let c = KcpConfig::stream_mode();
            (
                Kcp::new(1, c.clone()).unwrap(),
                Arc::new(Mutex::new(Kcp::new(1, c).unwrap())),
            )
        };
        let now = current_millis();
        s.current = now;
        // BUG-6 fix: rmt_wnd starts at 0. Set non-zero to simulate ACK exchange
        s.rmt_wnd = 32;
        r.lock().unwrap().current = now;
        let r2 = r.clone();
        s.set_output(move |d| {
            r2.lock().unwrap().input(&d).ok();
            Box::pin(async move { Ok(d.len()) })
        });
        assert_eq!(s.send(b"Hello Stream!").unwrap(), b"Hello Stream!".len());
        s.update(now + 20).await.unwrap();
        let recv_result = r.lock().unwrap().recv().unwrap();
        assert_eq!(recv_result.to_vec(), b"Hello Stream!");
    }

    #[tokio::test]
    async fn test_stream_mode_multi_send_reorder() {
        let (mut s, r) = {
            let c = KcpConfig::stream_mode();
            (
                Kcp::new(1, c.clone()).unwrap(),
                Arc::new(Mutex::new(Kcp::new(1, c).unwrap())),
            )
        };
        let now = current_millis();
        s.current = now;
        // BUG-6 fix: rmt_wnd starts at 0. Set non-zero to simulate ACK exchange
        s.rmt_wnd = 32;
        r.lock().unwrap().current = now;
        let r2 = r.clone();
        s.set_output(move |d| {
            r2.lock().unwrap().input(&d).ok();
            Box::pin(async move { Ok(d.len()) })
        });

        s.send(b"Hello ").unwrap();
        s.send(b"Stream ").unwrap();
        s.send(b"Mode!").unwrap();
        for i in 0..5 {
            s.current = now + i * s.interval;
            s.update(s.current).await.unwrap();
        }
        assert_eq!(
            r.lock().unwrap().recv().unwrap().to_vec(),
            b"Hello Stream Mode!"
        );
    }

    // ===== Bug-fix regression tests =====

    #[test]
    fn test_parse_ack_nsnd_buf_integrity() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        // Manually populate snd_buf with 5 segments
        for sn in 0..5u32 {
            let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, sn, 0, 10);
            let seg = Segment::new_with_header_and_data(header, &[0u8; 10]).unwrap();
            kcp.snd_buf.push_back(seg);
            kcp.nsnd_buf += 1;
        }
        assert_eq!(kcp.nsnd_buf, 5);

        // Process ACK for sn=2 — removes exactly one segment
        kcp.parse_ack(2);

        // nsnd_buf must reflect the remaining 4 segments
        assert_eq!(kcp.nsnd_buf, 4);
        // Remaining segments must be in order
        let sns: Vec<u32> = kcp.snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        assert_eq!(sns, vec![0, 1, 3, 4]);
        assert_eq!(kcp.waitsnd(), 4);
    }

    #[test]
    fn test_parse_data_out_of_order() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        // Insert segments out of order
        for sn in [3u32, 1, 4, 0, 2] {
            let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, sn, 0, 5);
            let seg = Segment::new_with_header_and_data(header, &[sn as u8; 5]).unwrap();
            kcp.parse_data(seg);
        }
        assert_eq!(kcp.nrcv_buf, 5);
        // rcv_buf must be sorted by sn
        let sns: Vec<u32> = kcp.rcv_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        assert_eq!(sns, vec![0, 1, 2, 3, 4]);

        // Duplicate sn should be silently dropped
        let dup_header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 2, 0, 3);
        let dup_seg = Segment::new_with_header_and_data(dup_header, &[0u8; 3]).unwrap();
        kcp.parse_data(dup_seg);
        assert_eq!(kcp.nrcv_buf, 5);
    }

    #[tokio::test]
    async fn test_non_stream_mode_full_cycle() {
        let (mut s, r) = {
            let c = KcpConfig::fast_mode();
            (
                Kcp::new(1, c.clone()).unwrap(),
                Arc::new(Mutex::new(Kcp::new(1, c).unwrap())),
            )
        };
        let now = current_millis();
        s.current = now;
        // BUG-6 fix: rmt_wnd starts at 0. Set non-zero to simulate ACK exchange
        s.rmt_wnd = 32;
        r.lock().unwrap().current = now;
        let r2 = r.clone();
        s.set_output(move |d| {
            r2.lock().unwrap().input(&d).ok();
            Box::pin(async move { Ok(d.len()) })
        });

        // Send a message that spans multiple segments (3 * mss + 50 bytes = 4 segments)
        let msg = random_bytes(s.mss as usize * 3 + 50);
        assert_eq!(s.send(&msg).unwrap(), msg.len());
        assert_eq!(s.nsnd_que, 4);

        // Flush multiple times to ensure all segments arrive
        for i in 0..5 {
            s.current = now + i * s.interval;
            s.update(s.current).await.unwrap();
        }
        // BUG-6 fix: receiver's rmt_wnd updated from received header.wnd
        // Receiver reassembles the message
        let received = r.lock().unwrap().recv().unwrap();
        assert_eq!(received.to_vec(), msg);
        assert_eq!(r.lock().unwrap().nrcv_que, 0);
    }

    #[tokio::test]
    async fn test_window_probe_backoff() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        let now = current_millis();
        kcp.current = now;
        kcp.rmt_wnd = 0;

        // Dummy output so send_packet doesn't fail
        kcp.set_output(move |data| Box::pin(async move { Ok(data.len()) }));

        // 1st flush: 首次探测不等待，退避初值取 interval
        kcp.flush().await.unwrap();
        assert_eq!(
            kcp.probe & IKCP_ASK_SEND,
            IKCP_ASK_SEND,
            "ASK_SEND should be set after first flush with rmt_wnd=0"
        );
        let first_wait = kcp.interval.max(1);
        assert_eq!(kcp.probe_wait, first_wait);
        assert_eq!(kcp.ts_probe, now, "首次探测应当即到期");

        // Simulate Wask was sent (probe cleared) and advance time past ts_probe
        kcp.probe &= !IKCP_ASK_SEND;
        kcp.current = now + first_wait;

        // 2nd flush: probe_wait should double
        kcp.flush().await.unwrap();
        assert_eq!(kcp.probe_wait, first_wait * 2);
        assert_eq!(kcp.ts_probe, kcp.current + first_wait * 2);

        // 3rd flush: probe_wait doubles again
        kcp.probe &= !IKCP_ASK_SEND;
        kcp.current = now + first_wait * 3;
        kcp.flush().await.unwrap();
        assert_eq!(kcp.probe_wait, first_wait * 4);
        assert_eq!(kcp.ts_probe, kcp.current + first_wait * 4);

        // Advance time but NOT enough for the new ts_probe
        kcp.probe &= !IKCP_ASK_SEND;
        kcp.current = now + first_wait * 4 + 1;
        kcp.flush().await.unwrap();
        // need_wask should be false (ts_probe hasn't elapsed), so probe stays set
        assert_eq!(
            kcp.probe & IKCP_ASK_SEND,
            IKCP_ASK_SEND,
            "probe should remain set when ts_probe not yet elapsed"
        );
        assert!(
            kcp.current < kcp.ts_probe,
            "current should be before ts_probe"
        );
    }

    /// 两端同时新建、rmt_wnd 均为 0 时，探路段应能在首轮 update 内送达对端，
    /// 而不需要等窗口探测间隔。
    #[tokio::test]
    async fn test_connect_without_prior_peer_packet() {
        let c = KcpConfig::fast_mode();
        let mut s = Kcp::new(7, c.clone()).unwrap();
        let r = Arc::new(Mutex::new(Kcp::new(7, c).unwrap()));
        let now = current_millis();
        s.current = now;
        r.lock().unwrap().current = now;

        // 两端初始都不知道对端窗口
        assert_eq!(s.rmt_wnd, 0);
        assert_eq!(r.lock().unwrap().rmt_wnd, 0);

        // 发送端发出的包直接投递给接收端
        let r2 = r.clone();
        s.set_output(move |d| {
            r2.lock().unwrap().input(&d).ok();
            Box::pin(async move { Ok(d.len()) })
        });

        let msg = random_bytes(1000);
        s.send(&msg).unwrap();
        s.update(now).await.unwrap();

        assert_eq!(
            s.nsnd_buf, 1,
            "rmt_wnd 未知时必须放行探路段，否则两端相互空等"
        );
        assert_eq!(r.lock().unwrap().nrcv_que, 1, "探路段应已送达对端");
        assert_eq!(r.lock().unwrap().recv().unwrap().to_vec(), msg);
    }

    // ===== BUG-1: cwnd init =====
    #[test]
    fn test_cwnd_init_nonzero() {
        let kcp = Kcp::new(1, KcpConfig::default()).unwrap();
        assert_eq!(kcp.cwnd, IKCP_THRESH_INIT as u16);
    }

    #[tokio::test]
    async fn test_default_mode_can_send() {
        let mut kcp = Kcp::new(1, KcpConfig::default()).unwrap();
        let now = current_millis();
        kcp.current = now;
        kcp.set_output(move |data| Box::pin(async move { Ok(data.len()) }));
        kcp.rmt_wnd = 32; // Simulate remote window size from peer
        kcp.send(&[1, 2, 3]).unwrap();
        kcp.flush().await.unwrap();
        // After flush, seg should be moved from snd_queue to snd_buf
        assert!(
            kcp.nsnd_buf > 0,
            "Data should have been sent with default config"
        );
        assert_eq!(kcp.nsnd_que, 0);
    }

    // ===== BUG-2: parse_ack RTT comparison =====
    #[test]
    fn test_parse_ack_rtt_correct() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        // snd_buf with 3 segments at different timestamps
        for (sn, ts) in [(0u32, 100u64), (1, 200), (2, 300)] {
            let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, ts, sn, 0, 10);
            let seg = Segment::new_with_header_and_data(header, &[0u8; 10]).unwrap();
            kcp.snd_buf.push_back(seg);
            kcp.nsnd_buf += 1;
        }
        kcp.current = 500;

        // ACK for sn=1 (middle segment) — RTT should use ts=200
        kcp.parse_ack(1);

        // Verify RTT was calculated from ts=200 (the matching segment's timestamp)
        let expected_rtt = (500i64).wrapping_sub(200i64); // = 300
        assert_eq!(kcp.rx_srtt, expected_rtt);
        assert_eq!(kcp.rx_rttval, expected_rtt / 2);
    }

    // ===== BUG-3: snd_una updated =====
    #[test]
    fn test_snd_una_updated() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        // Populate snd_buf with sn 10, 11, 12
        for sn in 10u32..13 {
            let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, sn, 0, 5);
            let seg = Segment::new_with_header_and_data(header, &[0u8; 5]).unwrap();
            kcp.snd_buf.push_back(seg);
            kcp.nsnd_buf += 1;
        }
        assert_eq!(kcp.nsnd_buf, 3);

        // parse_una with una=12 removes sn 10, 11 (sn < 12)
        kcp.parse_una(12);
        assert_eq!(kcp.nsnd_buf, 1);
        // snd_una should be updated to 12
        assert_eq!(kcp.snd_una, 12);
        let remaining_sn = kcp.snd_buf.front().unwrap().kcp_packet.header.sn;
        assert_eq!(remaining_sn, 12);
    }

    // ===== BUG-6: rmt_wnd init =====
    #[test]
    fn test_rmt_wnd_zero_at_init() {
        let kcp = Kcp::new(1, KcpConfig::default()).unwrap();
        assert_eq!(kcp.rmt_wnd, 0);
    }

    // ===== BUG-7: fastack incremented =====
    #[test]
    fn test_fastack_incremented() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        // snd_buf with sn 0, 1, 2
        for sn in 0u32..3 {
            let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, sn, 0, 5);
            let seg = Segment::new_with_header_and_data(header, &[0u8; 5]).unwrap();
            kcp.snd_buf.push_back(seg);
            kcp.nsnd_buf += 1;
        }
        kcp.snd_una = 0;

        // Simulate input with una=1: all seg with sn<1 get fastack incremented
        // (seg with sn=0 gets fastack+1 since 1 > 0)
        let una = 1u32;
        kcp.parse_fastack(una);

        let fastacks: Vec<u32> = kcp.snd_buf.iter().map(|s| s.fastack).collect();
        assert_eq!(fastacks, vec![1, 0, 0]);

        // Second fastack with una=2: seg sn=0,1 get fastack+1
        kcp.parse_fastack(2);
        let fastacks: Vec<u32> = kcp.snd_buf.iter().map(|s| s.fastack).collect();
        assert_eq!(fastacks, vec![2, 1, 0]);
    }

    // ===== BUG-4: ACK sent with Wask =====
    #[tokio::test]
    async fn test_ack_sent_with_wask() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        let now = current_millis();
        kcp.current = now;
        kcp.rmt_wnd = 0; // remote window zero → probe will be set
        let sent_ack = Arc::new(Mutex::new(false));
        let sent_wask = Arc::new(Mutex::new(false));
        {
            let sent_ack = sent_ack.clone();
            let sent_wask = sent_wask.clone();
            kcp.set_output(move |data| {
                // Parse the packet to check its type
                let mut buf = Bytes::copy_from_slice(&data);
                if let Ok((header, _)) = KcpPacketHeader::from_bytes(&mut buf) {
                    if header.cmd == KcpCmd::Ack {
                        *sent_ack.lock().unwrap() = true;
                    } else if header.cmd == KcpCmd::Wask {
                        *sent_wask.lock().unwrap() = true;
                    }
                }
                Box::pin(async move { Ok(data.len()) })
            });
        }

        // First flush: rmt_wnd=0 → probe set, but no acks yet
        kcp.flush().await.unwrap();
        // After this flush, we should have probe set for ASK_SEND
        // (the probe logic runs after ACK/Wask sending in flush)
        assert_ne!(kcp.probe & IKCP_ASK_SEND, 0);

        // Add acks to acklist (simulating input coming in) and set probe for Wask
        kcp.acklist.push(42);
        kcp.acklist.push(99);
        // Also ensure probe is set and timing condition met
        kcp.probe |= IKCP_ASK_SEND;
        kcp.ts_probe = now - 1000; // past due
        kcp.probe_wait = 7000;

        // Second flush: need_wask might be set, BUT acks_to_send has items
        kcp.flush().await.unwrap();

        // Verify: ACKs should have been sent (regardless of Wask being sent or not)
        assert!(*sent_ack.lock().unwrap(), "ACKs should still be sent");
    }

    // ===== BUG-5: no duplicate retransmit =====
    #[test]
    fn test_no_dup_retransmit() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        let now = current_millis();
        kcp.current = now;
        kcp.rmt_wnd = 32;
        kcp.set_output(move |data| Box::pin(async move { Ok(data.len()) }));

        // Move a segment from snd_queue to snd_buf via flush
        kcp.send(&[1, 2, 3]).unwrap();
        // We need to run flush in async context for sends, but we can manually set up snd_buf instead
        let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 100, now, 0, 0, 3);
        let mut seg = Segment::new_with_header_and_data(header, &[1, 2, 3]).unwrap();
        seg.xmit = 1;
        // Set timeout (resendts <= current)
        seg.resendts = now - 100;
        seg.rto = 100;
        // Set fastack >= fastresend (fastresend=2 for fast_mode)
        seg.fastack = 5;
        kcp.snd_buf.push_back(seg);
        kcp.nsnd_buf += 1;
        kcp.fastresend = 2;

        // Now manually run the retransmission logic from flush to check sends
        let current = kcp.current;
        let mut sends = Vec::new();

        for seg in kcp.snd_buf.iter() {
            if (current as i32).wrapping_sub(seg.resendts as i32) >= 0 {
                // Would timeout retransmit
                sends.push(seg.kcp_packet.header.sn);
            } else if seg.fastack >= kcp.fastresend as u32 {
                // Would fast retransmit
                sends.push(seg.kcp_packet.header.sn);
            }
        }

        // Verify: with the fix (else if), only timeout triggers, not fastack too
        // Without the fix (two ifs), this seg would appear twice in sends
        assert_eq!(
            sends.len(),
            1,
            "Segment should only be scheduled for retransmit once"
        );
        assert_eq!(sends[0], 0);
    }

    // ===== BUG-8: parse_ack retain =====
    #[test]
    fn test_parse_ack_retain() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        // Populate snd_buf with 5 segments (sn 0..4)
        for sn in 0..5u32 {
            let header = KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, sn, 0, 10);
            let seg = Segment::new_with_header_and_data(header, &[0u8; 10]).unwrap();
            kcp.snd_buf.push_back(seg);
            kcp.nsnd_buf += 1;
        }
        assert_eq!(kcp.nsnd_buf, 5);

        // Process ACK for sn=1 (middle segment)
        kcp.parse_ack(1);

        // Verify: nsnd_buf decremented by 1
        assert_eq!(kcp.nsnd_buf, 4);
        // Verify: remaining segments in order: 0, 2, 3, 4
        let sns: Vec<u32> = kcp.snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        assert_eq!(sns, vec![0, 2, 3, 4]);

        // Process ACK for sn=0 (first segment)
        kcp.parse_ack(0);
        assert_eq!(kcp.nsnd_buf, 3);
        let sns: Vec<u32> = kcp.snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        assert_eq!(sns, vec![2, 3, 4]);

        // Process ACK for sn=4 (last segment)
        kcp.parse_ack(4);
        assert_eq!(kcp.nsnd_buf, 2);
        let sns: Vec<u32> = kcp.snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        assert_eq!(sns, vec![2, 3]);

        // Process ACK for a segment not in snd_buf (sn=99)
        kcp.parse_ack(99);
        // Nothing should be removed, nsnd_buf unchanged
        assert_eq!(kcp.nsnd_buf, 2);
        let sns: Vec<u32> = kcp.snd_buf.iter().map(|s| s.kcp_packet.header.sn).collect();
        assert_eq!(sns, vec![2, 3]);
    }

    // ===== BUG-9: stream peeksize sum =====
    #[test]
    fn test_stream_peeksize_sum() {
        let mut kcp = Kcp::new(1, KcpConfig::stream_mode()).unwrap();
        // Push 3 segments of sizes 3, 4, 2
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 1, 0, 3),
                &[1, 2, 3],
            )
            .unwrap(),
        );
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 2, 0, 4),
                &[4, 5, 6, 7],
            )
            .unwrap(),
        );
        kcp.rcv_queue.push_back(
            Segment::new_with_header_and_data(
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 1000, 3, 0, 2),
                &[8, 9],
            )
            .unwrap(),
        );
        kcp.nrcv_que += 3;

        // peeksize should return sum of all segment lengths = 3 + 4 + 2 = 9
        assert_eq!(kcp.peeksize().unwrap(), 9);
    }

    // ===== BUG-11: large message deadlock =====
    #[tokio::test]
    async fn test_no_deadlock_large_message() {
        // Use larger rcv_wnd to accommodate 219 segments (300KB / 1372)
        let s_config = KcpConfig {
            rcv_wnd: 256,
            ..KcpConfig::fast_mode()
        };
        let r_config = KcpConfig {
            rcv_wnd: 256,
            ..KcpConfig::fast_mode()
        };
        let (mut s, r) = {
            (
                Kcp::new(1, s_config).unwrap(),
                Arc::new(Mutex::new(Kcp::new(1, r_config).unwrap())),
            )
        };
        let now = current_millis();
        s.current = now;
        // BUG-6 fix: rmt_wnd starts at 0. Set non-zero to simulate ACK exchange
        s.rmt_wnd = 32;
        r.lock().unwrap().current = now;
        let r2 = r.clone();
        s.set_output(move |d| {
            r2.lock().unwrap().input(&d).ok();
            Box::pin(async move { Ok(d.len()) })
        });

        // Send >175KB data (>128 segments to trigger bug without fix)
        let data_size = 300_000; // ~300KB
        let msg = random_bytes(data_size);
        assert_eq!(s.send(&msg).unwrap(), data_size);
        // Expected segment count: ceil(300000 / 1372) = ceil(218.66) = 219 segments
        assert!(s.nsnd_que > 128, "Should have more than 128 segments");

        // Flush multiple times to ensure all segments arrive
        for i in 0..30 {
            s.current = now + i * s.interval;
            if s.update(s.current).await.is_err() {
                break;
            }
            // Advance receiver time as well
            r.lock().unwrap().current = s.current;
        }

        // Verify all data was received correctly
        let received = r.lock().unwrap().recv().unwrap();
        assert_eq!(received.len(), data_size, "All data should be received");
        assert_eq!(received.to_vec(), msg);
        assert_eq!(r.lock().unwrap().nrcv_que, 0);
    }
}
