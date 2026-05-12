//! KCP控制块核心实现
//!
//! 本模块定义了KCP协议的核心控制块结构，对应C代码中的IKCPCB

use std::pin::Pin;

use bytes::{Buf, Bytes, BytesMut};

use crate::KcpCmd;
use crate::config::KcpConfig;
use crate::error::{IncompleteDataType, KcpError, KcpResult};
use crate::helper::{
    IKCP_ASK_SEND, IKCP_ASK_TELL, IKCP_DEADLINK, IKCP_FASTACK_LIMIT, IKCP_OVERHEAD, IKCP_RTO_DEF,
    IKCP_RTO_MAX, IKCP_RTO_MIN, IKCP_THRESH_INIT, IKCP_THRESH_MIN, KcpPacket, KcpPacketHeader,
    current_millis,
};
use crate::queue::{KcpDeque, Segment};

/// 输出回调函数类型别名
type OutputCallback = Box<
    dyn Fn(Bytes) -> Pin<Box<dyn Future<Output = KcpResult<usize>> + Send + 'static>> + Send + Sync,
>;

/// 日志回调函数类型别名
type LogCallback = Box<dyn Fn(&str, &Kcp) + Send + Sync>;

/// KCP控制块
///
/// KCP协议的核心控制结构，维护连接状态、窗口、队列等信息
///
/// # 字段说明
///
/// - **连接状态**: conv, state
/// - **序列号**: snd_una, snd_nxt, rcv_nxt
/// - **时间戳**: ts_recent, ts_lastack, current
/// - **窗口**: snd_wnd, rcv_wnd, rmt_wnd, cwnd
/// - **队列**: snd_queue, rcv_queue, snd_buf, rcv_buf
pub struct Kcp {
    /// 会话ID（必须两端一致）
    pub conv: u32,
    /// 最大传输单元
    pub mtu: u32,
    /// 最大分段大小
    pub mss: u32,
    /// 连接状态
    pub state: u32,

    // 发送序列号
    /// 未确认的最小序列号
    pub snd_una: u32,
    /// 下一个待发送的序列号
    pub snd_nxt: u32,
    /// 下一个期望接收的序列号
    pub rcv_nxt: u32,

    // 时间戳相关
    /// 最近接收到的数据包时间戳
    pub ts_recent: u64,
    /// 最近一次ACK的时间戳
    pub ts_lastack: u64,
    /// 慢启动阈值
    pub ssthresh: u32,

    // RTT和RTO
    /// RTT变化量估计值
    pub rx_rttval: i64,
    /// 平滑RTT估计值
    pub rx_srtt: i64,
    /// 重传超时时间
    pub rx_rto: u64,
    /// 最小RTO
    pub rx_minrto: u64,

    // 窗口相关
    /// 发送窗口大小
    pub snd_wnd: u16,
    /// 接收窗口大小
    pub rcv_wnd: u16,
    /// 远端窗口大小
    pub rmt_wnd: u16,
    /// 拥塞窗口
    pub cwnd: u16,
    /// 窗口探测标志
    pub probe: u32,

    // 定时相关
    /// 当前时钟（毫秒）
    pub current: u64,
    /// 内部更新间隔（毫秒）
    pub interval: u64,
    /// 下一次刷新时间戳
    pub ts_flush: u64,
    /// 发送计数
    pub xmit: u32,

    // 队列大小计数
    /// 接收缓冲区中的段数
    pub nrcv_buf: usize,
    /// 发送缓冲区中的段数
    pub nsnd_buf: usize,
    /// 接收队列中的段数
    pub nrcv_que: usize,
    /// 发送队列中的段数
    pub nsnd_que: usize,

    // 配置选项
    /// 是否启用无延迟模式
    pub nodelay: bool,
    /// 是否已经更新过
    pub updated: bool,
    /// 窗口探测时间戳
    pub ts_probe: u64,
    /// 窗口探测等待时间
    pub probe_wait: u64,
    /// 死链检测（超时重传次数）
    pub dead_link: u32,
    /// 拥塞窗口增量
    pub incr: u32,

    // 队列
    /// 发送队列（待发送的数据）
    pub snd_queue: KcpDeque,
    /// 接收队列（可读的有序数据）
    pub rcv_queue: KcpDeque,
    /// 发送缓冲区（已发送未确认）
    pub snd_buf: KcpDeque,
    /// 接收缓冲区（乱序到达的数据）
    pub rcv_buf: KcpDeque,

    // ACK列表
    /// ACK确认列表
    acklist: Vec<u32>,
    /// ACK列表容量
    #[allow(dead_code)]
    ackblock: usize,

    // 用户数据和回调
    /// 用户指针（透传给output回调）
    #[allow(dead_code)]
    user_data: Option<Box<dyn std::any::Any>>,
    /// 发送缓冲区
    buffer: Vec<u8>,
    /// 快速重传触发次数（0表示禁用）
    pub fastresend: i32,
    /// 快速重传限制
    pub fastlimit: i32,
    /// 是否禁用拥塞控制
    pub nocwnd: bool,

    // 日志配置
    /// 日志掩码
    pub logmask: u32,

    // 回调函数
    /// 输出回调函数：将KCP数据包发送给底层传输
    output: Option<OutputCallback>,
    /// 日志回调函数
    writelog: Option<LogCallback>,
}

unsafe impl Send for Kcp {}
unsafe impl Sync for Kcp {}

impl Kcp {
    /// 创建新的KCP控制块
    ///
    /// # 参数
    ///
    /// - `conv`: 会话ID，必须两端一致
    /// - `config`: KCP配置选项
    ///
    /// # 返回
    ///
    /// 返回新创建的Kcp实例或错误
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::{Kcp, KcpConfig};
    ///
    /// let kcp = Kcp::new(0x11223344, KcpConfig::default())?;
    /// ```
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
            rmt_wnd: config.rcv_wnd,
            cwnd: 0,
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
    /// # 参数
    ///
    /// - `callback`: 回调函数，接收数据和KCP引用，返回发送的字节数
    ///
    /// # 示例
    ///
    /// ```ignore
    /// kcp.set_output(|data, kcp| {
    ///     // 通过UDP发送data
    ///     udp_socket.send_to(data, &remote_addr)?;
    ///     Ok(data.len())
    /// });
    /// ```
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
    /// - `callback`: 日志回调函数
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

    /// 获取连接ID
    ///
    /// # 返回
    ///
    /// 返回当前KCP连接的会话ID
    pub fn getconv(&self) -> u32 {
        self.conv
    }

    /// 查询待发送数据包数量
    ///
    /// # 返回
    ///
    /// 返回发送队列和发送缓冲区中的总段数
    pub fn waitsnd(&self) -> usize {
        self.nsnd_que + self.nsnd_buf
    }

    // ========== 辅助函数 ==========

    /// 计算时间差
    ///
    /// 使用wrapping_sub处理32位时间戳回绕
    ///
    /// # 参数
    ///
    /// - `later`: 较晚的时间戳
    /// - `earlier`: 较早的时间戳
    #[inline]
    fn timediff_u32(&self, later: u32, earlier: u32) -> i32 {
        (later as i32).wrapping_sub(earlier as i32)
    }

    /// 计算时间差
    ///
    /// 使用wrapping_sub处理64位时间戳回绕
    ///
    /// # 参数
    ///
    /// - `later`: 较晚的时间戳
    /// - `earlier`: 较早的时间戳
    #[inline]
    fn timediff_u64(&self, later: u64, earlier: u64) -> i64 {
        (later as i64).wrapping_sub(earlier as i64)
    }

    // ========== 核心功能方法 ==========

    /// 发送数据
    ///
    /// 对应C代码的 ikcp_send() 函数 (ikcp.c:469)
    ///
    /// # 参数
    ///
    /// - `data`: 要发送的数据
    ///
    /// # 返回
    ///
    /// 返回发送的字节数，失败返回错误
    ///
    /// # 实现要点
    ///
    /// 1. 根据MSS分片数据
    /// 2. 设置每个segment的frg（分片索引）
    /// 3. 添加到snd_queue
    /// 4. 更新nsnd_que计数
    ///
    /// # 示例
    ///
    /// ```ignore
    /// kcp.send(b"Hello, KCP!")?;
    /// ```
    pub fn send(&mut self, data: &[u8]) -> KcpResult<usize> {
        let len = data.len();
        if len == 0 {
            return Ok(0);
        }

        let mut sent = 0;
        let mut buffer = data;

        // 计算需要多少个分片
        let count = buffer.len().div_ceil(self.mss as usize);

        // 由frg类型决定最大分片大小
        if count > u8::MAX as usize {
            return Err(KcpError::InvalidConfig(format!(
                "Too many fragments，data-size={}byte, allow-max-size={}byte",
                buffer.len(),
                (u8::MAX as usize) * (self.mss as usize)
            )));
        }

        // 分片发送
        for i in 0..count {
            let size = buffer.len().min(self.mss as usize);
            let header = KcpPacketHeader::new(
                self.conv,
                crate::KcpCmd::Push,
                (count.saturating_sub(i).saturating_sub(1)) as u8,
                self.rcv_wnd,
                current_millis(),
                self.snd_nxt,
                self.snd_una,
                size as u32,
            );
            let seg = Segment::new_with_header_and_data(header, &buffer[..size])?;

            self.snd_queue.push_back(seg);
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
            self.nsnd_que = self.nsnd_que.saturating_add(1);

            buffer = &buffer[size..];
            sent += size;
        }

        Ok(sent)
    }

    /// 查看下一个消息大小（不消耗数据）
    ///
    /// 对应C代码的 ikcp_peeksize() 函数 (ikcp.c:441)
    ///
    /// # 返回
    ///
    /// 返回下一个可读消息的字节数，如果没有数据返回错误
    pub fn peeksize(&self) -> KcpResult<usize> {
        if self.rcv_queue.is_empty() {
            return Err(KcpError::QueueEmpty);
        }

        let first = self.rcv_queue.front().unwrap();

        // 如果是最后一个分片
        if first.kcp_packet.header.frg == 0 {
            return Ok(first.kcp_packet.header.len as usize);
        }

        // 检查是否收到所有分片
        if self.nrcv_que < (first.kcp_packet.header.frg + 1) as usize {
            return Err(KcpError::IncompleteData(
                IncompleteDataType::PayloadWaitForFrg,
            ));
        }

        // 计算总大小
        let mut length = 0;
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
    /// 对应C代码的 ikcp_recv() 函数 (ikcp.c:365)
    ///
    /// # 参数
    ///
    /// - `buffer`: 接收缓冲区
    ///
    /// # 返回
    ///
    /// 返回接收到的字节数
    ///
    /// # 实现要点
    ///
    /// 1. 检查peeksize
    /// 2. 从rcv_queue读取数据并重组分片
    /// 3. 从rcv_buf移动有序数据到rcv_queue
    /// 4. 更新rcv_nxt和nrcv_que
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let mut recv_buf = [0u8; 4096];
    /// let len = kcp.recv(&mut recv_buf)?;
    /// println!("Received: {:?}", &recv_buf[..len]);
    /// ```
    pub fn recv(&mut self) -> KcpResult<Bytes> {
        let peeksize = self.peeksize()?;
        let mut buf = BytesMut::with_capacity(peeksize);
        // 从rcv_queue读取数据
        while let Some(seg) = self.rcv_queue.pop_front() {
            buf.extend_from_slice(&seg.get_data());
            self.nrcv_que -= 1;
            if seg.kcp_packet.header.frg == 0 {
                break;
            }
        }

        // 将 rcv_buf 中有序的段移入 rcv_queue
        while !self.rcv_buf.is_empty() {
            let front_sn = self.rcv_buf.front().unwrap().kcp_packet.header.sn;
            if front_sn == self.rcv_nxt && self.nrcv_que < self.rcv_wnd as usize {
                let seg = self.rcv_buf.pop_front().unwrap();
                self.nrcv_buf -= 1;
                self.rcv_queue.push_back(seg);
                self.nrcv_que += 1;
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            } else {
                break;
            }
        }

        // 快速恢复：请求对端告知窗口
        if self.nrcv_que < self.rcv_wnd as usize {
            self.probe |= IKCP_ASK_TELL;
        }

        if buf.len() != peeksize {
            return Err(KcpError::IncompleteData(IncompleteDataType::PayloadErr));
        }

        Ok(buf.copy_to_bytes(buf.len()))
    }

    /// 输入处理：解析接收到的数据包
    ///
    /// 对应C代码的 ikcp_input() 函数 (ikcp.c:556)
    ///
    /// # 参数
    ///
    /// - `data`: 从底层网络接收到的数据
    ///
    /// # 返回
    ///
    /// 成功返回Ok(())，失败返回错误
    ///
    /// # 实现要点
    ///
    /// 1. 解析数据包头部
    /// 2. 验证conv是否匹配
    /// 3. 根据cmd类型处理：
    ///    - PUSH: 数据包，更新rcv_buf
    ///    - ACK: 确认包，更新snd_buf
    ///    - WASK: 窗口探测请求
    ///    - WINS: 窗口大小通知
    /// 4. 更新RTT估计
    /// 5. 处理快速重传
    ///
    /// # 示例
    ///
    /// ```ignore
    /// // 从UDP接收到数据
    /// let (len, _) = udp_socket.recv_from(&mut buf)?;
    /// // 输入到KCP处理
    /// kcp.input(&buf[..len])?;
    /// ```
    pub fn input(&mut self, data: &[u8]) -> KcpResult<()> {
        if data.len() < IKCP_OVERHEAD {
            return Err(KcpError::IncompleteData(IncompleteDataType::Header));
        }
        let mut buf = Bytes::copy_from_slice(data);

        // 解析头部
        let (header, buf) = match KcpPacketHeader::from_bytes(&mut buf) {
            Ok((header, buf)) => (header, buf),
            Err(e) => return Err(e),
        };

        // 验证conv是否匹配
        if self.conv == 0 {
            self.conv = header.conv;
        } else if header.conv != self.conv {
            return Ok(()); // 静默丢弃不匹配的数据包
        }

        // 更新远端窗口大小
        self.rmt_wnd = header.wnd;

        // 处理una（确认所有sn < una的段）
        if self.timediff_u32(header.una, self.snd_una) > 0 {
            self.parse_una(header.una);
        }

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
                // PUSH命令：数据包
                // 检查序列号是否有效
                if self.timediff_u64(
                    header.sn as u64,
                    self.rcv_nxt as u64 + (self.rcv_wnd as u64),
                ) >= 0
                    || self.timediff_u64(header.sn as u64, self.rcv_nxt as u64) < 0
                {
                    // 超出窗口或已接收，丢弃
                    return Ok(());
                }

                // 重复检测
                for seg in self.rcv_buf.iter() {
                    if seg.kcp_packet.header.sn == header.sn {
                        return Ok(()); // 静默丢弃重复的数据包
                    }
                }

                // 创建segment
                let newseg = Segment::new_with_header_and_data(header, &buf)?;

                // 插入到rcv_buf
                self.parse_data(newseg);

                // 将该段的 sn 加入 acklist，供 flush 时发送 ACK
                self.acklist.push(header.sn);

                // 将 rcv_buf 中有序段移入 rcv_queue
                while !self.rcv_buf.is_empty() {
                    let front_sn = self.rcv_buf.front().unwrap().kcp_packet.header.sn;
                    if front_sn == self.rcv_nxt && self.nrcv_que < self.rcv_wnd as usize {
                        let seg = self.rcv_buf.pop_front().unwrap();
                        self.nrcv_buf -= 1;
                        self.nrcv_que += 1;
                        self.rcv_queue.push_back(seg);
                        self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    } else {
                        break;
                    }
                }
            }
            KcpCmd::Wask => {
                // 对方请求窗口信息
                self.probe |= IKCP_ASK_TELL;
            }
            KcpCmd::Wins => {
                // 仅更新了 wnd，无需额外处理
            }
        }

        // 任何包都可能通过 una 带来确认信息，提升快速重传计数
        if self.timediff_u32(header.una, self.snd_una) > 0 {
            self.parse_fastack(header.una);
        }

        // 更新时间戳
        if self.timediff_u64(self.current, header.ts) >= 10000
            || self.timediff_u64(header.ts, self.ts_recent) >= 10000
        {
            self.ts_recent = header.ts;
        }

        Ok(())
    }

    /// 移除所有 sn < una 的已确认段
    fn parse_una(&mut self, una: u32) {
        while !self.snd_buf.is_empty() {
            let front_sn = self.snd_buf.front().unwrap().kcp_packet.header.sn;
            if self.timediff_u32(una, front_sn) > 0 {
                self.snd_buf.pop_front();
                self.nsnd_buf -= 1;
            } else {
                break;
            }
        }
    }

    /// 处理 ACK 确认：更新 RTT 并删除被确认的段
    fn parse_ack(&mut self, sn: u32) {
        let current = self.current;
        let mut rtt_update = None;

        // 1. 计算 RTT（只对第一个匹配的段）
        for seg in self.snd_buf.iter() {
            let seg_sn = seg.kcp_packet.header.sn;
            if self.timediff_u32(sn, seg_sn) >= 0 {
                let rtt = self.timediff_u64(current, seg.kcp_packet.header.ts);
                if rtt >= 0 {
                    if self.rx_srtt == 0 {
                        rtt_update = Some((rtt, rtt / 2));
                    } else {
                        let delta = rtt - self.rx_srtt;
                        let new_srtt = self.rx_srtt + delta / 8;
                        let new_rttval = if delta < 0 {
                            self.rx_rttval + (-delta) / 4
                        } else {
                            self.rx_rttval + delta / 4
                        };
                        rtt_update = Some((new_srtt, new_rttval));
                    }
                }
                break; // 只取第一个匹配段
            }
        }

        if let Some((srtt, rttval)) = rtt_update {
            self.rx_srtt = srtt;
            self.rx_rttval = rttval;
            let rto_numerator = (srtt as u64) + self.interval.max(4 * rttval as u64);
            self.rx_rto = rto_numerator.max(self.rx_minrto).min(IKCP_RTO_MAX);
        }

        // 2. 从 snd_buf 中移除所有 sn == 指定值的段（通常只有一个）
        let mut removed = false;
        // 由于删除操作可能改变索引，我们重建队列
        let mut keep = Vec::with_capacity(self.nsnd_buf);
        while let Some(seg) = self.snd_buf.pop_front() {
            if seg.kcp_packet.header.sn == sn && !removed {
                removed = true;
                self.nsnd_buf -= 1;
                // 已确认的段不需要保留
            } else {
                self.nsnd_buf -= 1; // 因为从队列移除时会减少计数，这里临时修正
                keep.push(seg);
            }
        }
        // 再将保留的段放回
        for seg in keep {
            self.snd_buf.push_back(seg);
            self.nsnd_buf += 1;
        }
    }

    /// 根据 una 对所有未确认段增加快速重传计数
    fn parse_fastack(&mut self, una: u32) {
        for seg in self.snd_buf.iter_mut() {
            // 内联计算时间差以避免对 self 的不可变借用
            if (una as i32).wrapping_sub(seg.kcp_packet.header.sn as i32) > 0 && seg.fastack < 255 {
                seg.fastack += 1;
            }
        }
    }

    /// 将接收到的数据段插入 rcv_buf（保持按 sn 升序）
    fn parse_data(&mut self, newseg: Segment) {
        if self.nrcv_buf >= self.rcv_wnd as usize {
            return;
        }

        // 按序列号寻找插入位置
        let mut insert_pos = self.rcv_buf.len();
        for (i, seg) in self.rcv_buf.iter().enumerate() {
            if newseg.kcp_packet.header.sn == seg.kcp_packet.header.sn {
                return; // 重复包
            }
            if self.timediff_u32(newseg.kcp_packet.header.sn, seg.kcp_packet.header.sn) < 0 {
                insert_pos = i;
                break;
            }
        }

        // 将新段插入指定位置
        let mut tail = Vec::new();
        for _ in insert_pos..self.rcv_buf.len() {
            tail.push(self.rcv_buf.pop_back().unwrap());
        }
        self.rcv_buf.push_back(newseg);
        while let Some(seg) = tail.pop() {
            self.rcv_buf.push_back(seg);
        }

        self.nrcv_buf += 1;
    }

    /// 刷新发送缓冲区
    ///
    /// 对应C代码的 ikcp_flush() 函数 (ikcp.c:790)
    ///
    /// # 实现要点
    ///
    /// 1. 从snd_queue移动数据到snd_buf
    /// 2. 编码数据包（头部 + 数据）
    /// 3. 调用output回调发送
    /// 4. 更新发送序列号
    /// 5. 发送ACK列表
    /// 6. 发送窗口探测
    async fn flush(&mut self) -> KcpResult<()> {
        let window = self.snd_wnd.min(self.rmt_wnd);
        let mut can_send = window as usize;
        let mut lost = false; // 是否有数据包丢失

        // 检查拥塞窗口
        if !self.nocwnd {
            can_send = window.min(self.cwnd) as usize;
        }

        // 从 snd_queue 移入 snd_buf
        while can_send > 0 && !self.snd_queue.is_empty() {
            let mut seg = self.snd_queue.pop_front().unwrap();
            self.nsnd_que -= 1;

            // 设置
            seg.kcp_packet.header.conv = self.conv;
            seg.kcp_packet.header.cmd = KcpCmd::Push;
            seg.kcp_packet.header.wnd = self.rcv_wnd;
            seg.kcp_packet.header.ts = self.current;
            seg.kcp_packet.header.una = self.rcv_nxt; // 很重要：una 是接收窗口的下一期望序列号
            seg.resendts = self.current;
            seg.rto = self.rx_rto;
            seg.fastack = 0;
            seg.xmit = 0;

            self.snd_buf.push_back(seg);
            self.nsnd_buf += 1;
            can_send -= 1;
        }

        // 发送 ACK
        let acks_to_send = self.acklist.clone();
        self.acklist.clear();

        // 发送窗口探测（如果需要）
        let need_wask = self.acklist.is_empty() && (self.probe & IKCP_ASK_SEND) != 0;
        let need_wins = (self.probe & IKCP_ASK_TELL) != 0;

        // 发送ACK和探测窗口
        if need_wask {
            // 窗口探测请求
            self.send_packet(KcpPacket::new_without_data(KcpPacketHeader::new(
                self.conv,
                KcpCmd::Wask,
                0,
                self.rcv_wnd,
                current_millis(),
                0,
                self.rcv_nxt,
                0,
            )))
            .await?;
        } else {
            // 发送 ACK 段（每个被确认的 sn）
            for sn in acks_to_send {
                self.send_packet(KcpPacket::new_without_data(KcpPacketHeader::new(
                    self.conv,
                    KcpCmd::Ack,
                    0,
                    self.rcv_wnd,
                    current_millis(),
                    sn,
                    self.rcv_nxt,
                    0,
                )))
                .await?;
            }
        }

        if need_wins {
            self.send_packet(KcpPacket::new_without_data(KcpPacketHeader::new(
                self.conv,
                KcpCmd::Wins,
                0,
                self.rcv_wnd,
                current_millis(),
                0,
                self.rcv_nxt,
                0,
            )))
            .await?;
            self.probe &= !IKCP_ASK_TELL;
        }

        // 检查是否需要发送窗口探测
        if self.rmt_wnd == 0 && (self.probe & IKCP_ASK_SEND) == 0 {
            self.probe |= IKCP_ASK_SEND;
            self.probe_wait = 0; // 按需初始化
            self.ts_probe = self.current + self.probe_wait;
        }

        let current = self.current;

        // 收集需要发送（重传）的段
        let mut sends = Vec::new();
        let mut need_update = Vec::new();

        for (idx, seg) in self.snd_buf.iter().enumerate() {
            if seg.xmit == 0 {
                // 首次发送
                need_update.push((idx, 1, current.wrapping_add(seg.rto), seg.rto, false));
                sends.push((
                    seg.kcp_packet.header.frg,
                    seg.kcp_packet.header.sn,
                    seg.kcp_packet.header.ts,
                    seg.kcp_packet.header.una,
                    seg.kcp_packet.header.wnd,
                    seg.get_data(),
                ));
            } else {
                // 检查超时
                let diff = (current as i32).wrapping_sub(seg.resendts as i32);
                if diff >= 0 {
                    // 超时重传
                    if seg.xmit >= self.dead_link {
                        // 超过最大重传次数，连接断开
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
                        true, // 超时算作丢失
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

            // 快速重传
            if seg.fastack >= self.fastresend as u32 {
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
                    true, // 快速重传也标记为丢失
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

        // 实际发送
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

        // 更新段状态
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

        // 拥塞窗口调整
        if lost {
            self.ssthresh = (self.cwnd as u32 / 2).max(IKCP_THRESH_MIN);
            self.cwnd = 1;
            self.incr = 0;
        } else if self.timediff_u64(current, self.ts_lastack) >= 0 {
            // 正常，增加拥塞窗口
            if (self.cwnd as u32) < self.ssthresh {
                // 慢启动
                self.incr += self.mss;
            } else {
                // 拥塞避免
                self.incr += if self.mss > 0 {
                    self.mss * self.mss / (self.cwnd as u32)
                } else {
                    0
                };
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

    /// 更新 KCP 状态
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
    /// 对应C代码的 ikcp_check() 函数 (ikcp.c:1059)
    ///
    /// # 参数
    ///
    /// - `current`: 当前时间戳（毫秒）
    ///
    /// # 返回
    ///
    /// 返回下次需要更新的时间戳
    ///
    /// # 实现要点
    ///
    /// 检查是否有待发送的数据，计算最早的重传时间
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let next_update = kcp.check(current_timestamp_ms());
    /// ```
    pub fn check(&self, current: u64) -> u64 {
        let mut ts_flush = self.ts_flush;

        if !self.snd_queue.is_empty() || !self.snd_buf.is_empty() {
            for seg in self.snd_buf.iter() {
                let diff = self.timediff_u64(seg.resendts, current);
                if diff < 0 {
                    return current;
                } else if diff < self.timediff_u64(ts_flush, current) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::KcpCmd;

    #[test]
    fn test_kcp_new() {
        let config = KcpConfig::default();
        let kcp = Kcp::new(0x11223344, config);
        assert!(kcp.is_ok());

        let kcp = kcp.unwrap();
        assert_eq!(kcp.conv, 0x11223344);
        assert_eq!(kcp.mtu, 1400);
        assert_eq!(kcp.mss, 1372);
    }

    #[test]
    fn test_kcp_cmd_try_from() {
        assert_eq!(KcpCmd::try_from(81), Ok(KcpCmd::Push));
        assert_eq!(KcpCmd::try_from(82), Ok(KcpCmd::Ack));
        assert_eq!(KcpCmd::try_from(83), Ok(KcpCmd::Wask));
        assert_eq!(KcpCmd::try_from(84), Ok(KcpCmd::Wins));
        assert!(KcpCmd::try_from(99).is_err());
    }

    #[test]
    fn test_waitsnd() {
        let config = KcpConfig::default();
        let kcp = Kcp::new(0x11223344, config).unwrap();
        assert_eq!(kcp.waitsnd(), 0);
    }
}
