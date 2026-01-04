//! KCP控制块核心实现
//!
//! 本模块定义了KCP协议的核心控制块结构，对应C代码中的IKCPCB

use crate::config::KcpConfig;
use crate::error::{KcpError, KcpResult};
use crate::queue::{KcpDeque, Segment};

// KCP协议常量定义
const IKCP_CMD_PUSH: u32 = 81; // 推送数据
const IKCP_CMD_ACK: u32 = 82; // 确认
const IKCP_CMD_WASK: u32 = 83; // 窗口探测请求
const IKCP_CMD_WINS: u32 = 84; // 窗口大小通知

const IKCP_ASK_SEND: u32 = 1; // 需要发送窗口探测
const IKCP_ASK_TELL: u32 = 2; // 需要告知窗口大小

const IKCP_OVERHEAD: u32 = 24; // 包头大小
const IKCP_DEADLINK: u32 = 20; // 最大重传次数
const IKCP_THRESH_INIT: u32 = 2; // 初始慢启动阈值
#[allow(dead_code)]
const IKCP_THRESH_MIN: u32 = 2; // 最小慢启动阈值
const IKCP_FASTACK_LIMIT: i32 = 5; // 快速重传限制

#[allow(dead_code)]
const IKCP_RTO_NDL: u32 = 30; // 无延迟最小RTO
const IKCP_RTO_MIN: u32 = 100; // 最小RTO
const IKCP_RTO_DEF: u32 = 200; // 默认RTO
const IKCP_RTO_MAX: u32 = 60000; // 最大RTO

#[allow(dead_code)]
const IKCP_WND_SND: u32 = 32; // 默认发送窗口
const IKCP_WND_RCV: u32 = 128; // 默认接收窗口
#[allow(dead_code)]
const IKCP_MTU_DEF: u32 = 1400; // 默认MTU
#[allow(dead_code)]
const IKCP_INTERVAL: u32 = 100; // 默认更新间隔

/// 输出回调函数类型别名
type OutputCallback = Box<dyn Fn(&[u8], &mut Kcp) -> KcpResult<usize> + Send + Sync>;

/// 日志回调函数类型别名
type LogCallback = Box<dyn Fn(&str, &Kcp) + Send + Sync>;

/// KCP协议命令类型
///
/// 定义了KCP协议支持的4种命令类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KcpCmd {
    /// 推送数据命令
    Push = 81,
    /// 确认命令
    Ack = 82,
    /// 窗口探测请求
    Wask = 83,
    /// 窗口大小通知
    Wins = 84,
}

impl TryFrom<u32> for KcpCmd {
    type Error = KcpError;

    fn try_from(value: u32) -> KcpResult<Self> {
        match value {
            81 => Ok(KcpCmd::Push),
            82 => Ok(KcpCmd::Ack),
            83 => Ok(KcpCmd::Wask),
            84 => Ok(KcpCmd::Wins),
            _ => Err(KcpError::InvalidCommand(value)),
        }
    }
}

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
    pub ts_recent: u32,
    /// 最近一次ACK的时间戳
    pub ts_lastack: u32,
    /// 慢启动阈值
    pub ssthresh: u32,

    // RTT和RTO
    /// RTT变化量估计值
    pub rx_rttval: i32,
    /// 平滑RTT估计值
    pub rx_srtt: i32,
    /// 重传超时时间
    pub rx_rto: u32,
    /// 最小RTO
    pub rx_minrto: u32,

    // 窗口相关
    /// 发送窗口大小
    pub snd_wnd: u32,
    /// 接收窗口大小
    pub rcv_wnd: u32,
    /// 远端窗口大小
    pub rmt_wnd: u32,
    /// 拥塞窗口
    pub cwnd: u32,
    /// 窗口探测标志
    pub probe: u32,

    // 定时相关
    /// 当前时钟（毫秒）
    pub current: u32,
    /// 内部更新间隔（毫秒）
    pub interval: u32,
    /// 下一次刷新时间戳
    pub ts_flush: u32,
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
    pub ts_probe: u32,
    /// 窗口探测等待时间
    pub probe_wait: u32,
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
    /// 是否流式模式
    pub stream: bool,

    // 日志配置
    /// 日志掩码
    pub logmask: u32,

    // 回调函数
    /// 输出回调函数：将KCP数据包发送给底层传输
    output: Option<OutputCallback>,
    /// 日志回调函数
    writelog: Option<LogCallback>,
}

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
        let mss = mtu - IKCP_OVERHEAD; // IKCP_OVERHEAD = 24

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
            buffer: vec![0u8; (mtu as usize + IKCP_OVERHEAD as usize) * 3],
            fastresend: config.fastresend,
            fastlimit: IKCP_FASTACK_LIMIT,
            nocwnd: config.nocwnd,
            stream: config.stream,

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
    pub fn set_output<F>(&mut self, callback: F)
    where
        F: Fn(&[u8], &mut Kcp) -> KcpResult<usize> + Send + Sync + 'static,
    {
        self.output = Some(Box::new(callback));
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
    fn timediff(&self, later: u32, earlier: u32) -> i32 {
        (later as i32).wrapping_sub(earlier as i32)
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
    /// 1. 流式模式：尝试追加到snd_queue的最后一个segment
    /// 2. 根据MSS分片数据
    /// 3. 设置每个segment的frg（分片索引）
    /// 4. 添加到snd_queue
    /// 5. 更新nsnd_que计数
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

        // 流式模式：尝试追加到最后一个segment
        if self.stream {
            if let Some(last) = self.snd_queue.back_mut()
                && last.len < self.mss
            {
                let capacity = (self.mss - last.len) as usize;
                let extend = buffer.len().min(capacity);

                last.data.extend_from_slice(&buffer[..extend]);
                last.len += extend as u32;
                last.frg = 0;

                buffer = &buffer[extend..];
                sent += extend;
            }

            if buffer.is_empty() {
                return Ok(sent);
            }
        }

        // 计算需要多少个分片
        let count = buffer.len().div_ceil(self.mss as usize);

        if count >= IKCP_WND_RCV as usize {
            if self.stream && sent > 0 {
                return Ok(sent);
            }
            return Err(KcpError::InvalidConfig("Too many fragments".to_string()));
        }

        // 分片发送
        for i in 0..count {
            let size = buffer.len().min(self.mss as usize);
            let mut seg = Segment::new(buffer[..size].to_vec());
            seg.conv = self.conv;
            seg.cmd = IKCP_CMD_PUSH;
            seg.frg = if !self.stream {
                (count - i - 1) as u32
            } else {
                0
            };
            seg.len = size as u32;
            seg.wnd = self.rcv_wnd;

            self.snd_queue.push_back(seg);
            self.nsnd_que += 1;

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
        if first.frg == 0 {
            return Ok(first.len as usize);
        }

        // 检查是否收到所有分片
        if self.nrcv_que < (first.frg + 1) as usize {
            return Err(KcpError::IncompleteData);
        }

        // 计算总大小
        let mut length = 0;
        for seg in self.rcv_queue.iter() {
            length += seg.len as usize;
            if seg.frg == 0 {
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
    pub fn recv(&mut self, buffer: &mut [u8]) -> KcpResult<usize> {
        let peeksize = self.peeksize()?;
        if buffer.len() < peeksize {
            return Err(KcpError::BufferTooSmall);
        }

        let mut len = 0;
        let recover = !self.rcv_queue.is_empty();

        // 从rcv_queue读取数据
        while let Some(seg) = self.rcv_queue.pop_front() {
            let start = len;
            let end = len + seg.data.len();
            buffer[start..end].copy_from_slice(&seg.data);
            len = end;
            self.nrcv_que -= 1;

            if seg.frg == 0 {
                break;
            }
        }

        assert_eq!(len, peeksize);

        // 从rcv_buf移动有序数据到rcv_queue
        while !self.rcv_buf.is_empty() {
            let front_sn = self.rcv_buf.front().unwrap().sn;
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

        // 快速恢复
        if self.nrcv_que < self.rcv_wnd as usize && recover {
            self.probe |= IKCP_ASK_TELL;
        }

        Ok(len)
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
    /// 1. 解析数据包头部（24字节）
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
        if data.len() < IKCP_OVERHEAD as usize {
            return Err(KcpError::IncompleteData);
        }

        let mut offset = 0;

        // 解析头部（24字节）
        // 0-3: conv (会话ID)
        let conv = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        offset += 4;

        // 4: cmd (命令)
        let cmd = data[offset] as u32;
        offset += 1;

        // 5-6: wnd (窗口大小)
        let wnd = u16::from_be_bytes([data[offset], data[offset + 1]]) as u32;
        offset += 2;

        // 7-8: unused (保留字段)
        offset += 2;

        // 9-12: ts (时间戳)
        let ts = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // 13-16: sn (序列号)
        let sn = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // 17-20: una (未确认的最小序列号)
        let una = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // 21-24: len (数据长度)
        let len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // 验证conv是否匹配
        if conv != self.conv {
            return Ok(()); // 静默丢弃不匹配的数据包
        }

        // 更新远端窗口大小
        self.rmt_wnd = wnd;

        // 处理una（确认所有sn < una的段）
        if self.timediff(sn, self.snd_una) > 0 || self.timediff(una, self.snd_una) > 0 {
            self.parse_una(una);
            self.parse_ack(sn);
        }

        // 根据命令类型处理
        match cmd {
            IKCP_CMD_ACK => {
                // ACK命令：确认包
                // parse_ack已经在上面处理了
                if let Some(ref writelog) = self.writelog {
                    writelog(&format!("input ack: sn={}", sn), self);
                }
            }
            IKCP_CMD_PUSH => {
                // PUSH命令：数据包
                // 检查序列号是否有效
                if self.timediff(sn, self.rcv_nxt + self.rcv_wnd) >= 0
                    || self.timediff(sn, self.rcv_nxt) < 0
                {
                    // 超出窗口或已接收，丢弃
                    return Ok(());
                }

                // 提取数据
                let segment_data = if len > 0 {
                    if offset + len as usize > data.len() {
                        return Err(KcpError::IncompleteData);
                    }
                    data[offset..offset + len as usize].to_vec()
                } else {
                    Vec::new()
                };

                // 创建segment
                let mut newseg = Segment::new(segment_data);
                newseg.conv = conv;
                newseg.cmd = cmd;
                newseg.wnd = wnd;
                newseg.ts = ts;
                newseg.sn = sn;
                newseg.una = una;
                newseg.len = len;

                // 检查是否重复
                let mut duplicate = false;
                for seg in self.rcv_buf.iter() {
                    if seg.sn == sn {
                        duplicate = true;
                        break;
                    }
                }

                if !duplicate {
                    // 插入到rcv_buf
                    self.parse_data(newseg);
                }

                // 更新rcv_nxt
                while !self.rcv_buf.is_empty() {
                    let front_sn = self.rcv_buf.front().unwrap().sn;
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
            }
            IKCP_CMD_WASK => {
                // WASK命令：窗口探测请求
                self.probe |= IKCP_ASK_TELL;
            }
            IKCP_CMD_WINS => {
                // WINS命令：窗口大小通知
                // 不需要处理，wnd已经在上面更新
            }
            _ => {
                // 未知命令，忽略
            }
        }

        // 更新时间戳
        if self.timediff(self.current, ts) >= 10000 || self.timediff(ts, self.ts_recent) >= 10000 {
            self.ts_recent = ts;
        }

        Ok(())
    }

    /// 解析una：确认所有sn < una的段
    ///
    /// 对应C代码的 ikcp_parse_una() 函数
    fn parse_una(&mut self, una: u32) {
        while !self.snd_buf.is_empty() {
            let front_sn = self.snd_buf.front().unwrap().sn;
            if self.timediff(una, front_sn) > 0 {
                self.snd_buf.pop_front();
                self.nsnd_buf -= 1;
            } else {
                break;
            }
        }
    }

    /// 解析ack：确认特定的序列号
    ///
    /// 对应C代码的 ikcp_parse_ack() 函数
    fn parse_ack(&mut self, sn: u32) {
        // 先收集需要修改的segment的索引
        let current = self.current;
        let mut rx_srtt = self.rx_srtt;
        let mut rx_rttval = self.rx_rttval;
        let interval = self.interval;
        let rx_minrto = self.rx_minrto;

        let mut rtt_update = None;

        // 第一次遍历：计算RTT
        for seg in self.snd_buf.iter() {
            let seg_sn = seg.sn;
            if self.timediff(sn, seg_sn) >= 0 {
                // 计算RTT
                let rtt = self.timediff(current, seg.ts);

                if rtt >= 0 {
                    if rx_srtt == 0 {
                        rx_srtt = rtt;
                        rx_rttval = rtt / 2;
                    } else {
                        let delta = rtt - rx_srtt;
                        rx_srtt += delta / 8;
                        if delta < 0 {
                            rx_rttval += (-delta) / 4;
                        } else {
                            rx_rttval += delta / 4;
                        }
                    }

                    // 计算RTO
                    let rto_numerator = rx_srtt + interval.max((4 * rx_rttval) as u32) as i32;
                    let rto = rx_minrto.clamp(rto_numerator as u32, IKCP_RTO_MAX);

                    rtt_update = Some((rx_srtt, rx_rttval, rto));
                }
            }
        }

        // 应用RTT更新
        if let Some((srtt, rttval, rto)) = rtt_update {
            self.rx_srtt = srtt;
            self.rx_rttval = rttval;
            self.rx_rto = rto;
        }

        // 第二次遍历：增加快速重传计数
        for seg in self.snd_buf.iter_mut() {
            let seg_sn = seg.sn;
            // 内联时间差计算以避免借用检查器冲突
            let diff = (sn as i32).wrapping_sub(seg_sn as i32);
            if diff < 0 {
                // 增加快速重传计数
                if seg.fastack < 255 {
                    seg.fastack += 1;
                }
            }
        }
    }

    /// 解析数据：将接收到的数据插入rcv_buf
    ///
    /// 对应C代码的 ikcp_parse_data() 函数
    fn parse_data(&mut self, newseg: Segment) {
        // 检查是否超出接收窗口
        if self.nrcv_buf >= self.rcv_wnd as usize {
            return;
        }

        // 按序列号插入到正确位置
        let mut insert_pos = self.rcv_buf.len();

        for (i, seg) in self.rcv_buf.iter().enumerate() {
            if newseg.sn == seg.sn {
                // 重复包，丢弃
                return;
            }

            if self.timediff(newseg.sn, seg.sn) > 0 {
                insert_pos = i;
                break;
            }
        }

        // 插入segment
        if insert_pos < self.rcv_buf.len() {
            let mut temp = Vec::new();
            for _ in insert_pos..self.rcv_buf.len() {
                if let Some(seg) = self.rcv_buf.pop_back() {
                    temp.push(seg);
                }
            }
            self.rcv_buf.push_back(newseg);
            while let Some(seg) = temp.pop() {
                self.rcv_buf.push_back(seg);
            }
        } else {
            self.rcv_buf.push_back(newseg);
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
    /// 2. 编码数据包（24字节头部 + 数据）
    /// 3. 调用output回调发送
    /// 4. 更新发送序列号
    /// 5. 发送ACK列表
    /// 6. 发送窗口探测
    fn flush(&mut self) -> KcpResult<()> {
        // 计算可以发送的窗口大小
        let window = self.snd_wnd.min(self.rmt_wnd);
        let mut can_send = window as usize;
        let mut lost = false; // 是否有数据包丢失

        // 检查拥塞窗口
        if !self.nocwnd {
            can_send = window.min(self.cwnd) as usize;
        }

        // 从snd_queue移动数据到snd_buf
        while can_send > 0 && !self.snd_queue.is_empty() {
            let mut seg = self.snd_queue.pop_front().unwrap();
            self.nsnd_que -= 1;

            // 设置序列号
            seg.conv = self.conv;
            seg.cmd = IKCP_CMD_PUSH;
            seg.wnd = self.rcv_wnd;
            seg.ts = self.current;
            seg.sn = self.snd_nxt;
            seg.una = self.snd_una;
            seg.resendts = self.current;
            seg.rto = self.rx_rto;
            seg.fastack = 0;
            seg.xmit = 0;

            self.snd_nxt = self.snd_nxt.wrapping_add(1);
            self.snd_buf.push_back(seg);
            self.nsnd_buf += 1;

            can_send -= 1;
        }

        // 计算需要发送的ACK数量
        let _ackcount = self.acklist.len() / 2;

        // 收集需要发送的ACK
        let acks_to_send: Vec<(u32, u32)> = self
            .acklist
            .chunks(2)
            .filter_map(|chunk| {
                if chunk.len() == 2 {
                    Some((chunk[0], chunk[1]))
                } else {
                    None
                }
            })
            .collect();
        self.acklist.clear();

        // 发送窗口探测（如果需要）
        let need_wask = self.acklist.is_empty() && (self.probe & IKCP_ASK_SEND) != 0;
        let need_wins = (self.probe & IKCP_ASK_TELL) != 0;

        // 发送ACK和探测窗口
        if need_wask {
            // 发送窗口探测
            self.send_segment(IKCP_CMD_WASK, 0, 0, 0, 0, Vec::new())?;
        } else {
            // 发送ACK
            for (sn, ts) in acks_to_send {
                self.send_segment(IKCP_CMD_ACK, sn, ts, 0, 0, Vec::new())?;
            }
        }

        if need_wins {
            self.send_segment(IKCP_CMD_WINS, 0, 0, 0, 0, Vec::new())?;
            self.probe &= !IKCP_ASK_TELL;
        }

        // 计算重传（分两步进行以避免借用检查器冲突）
        let current = self.current;

        // 第一步：收集需要发送的segment信息和需要更新的字段
        let mut sends = Vec::new();
        let mut need_update = Vec::new();

        for (idx, seg) in self.snd_buf.iter().enumerate() {
            // 检查是否需要重传
            if seg.xmit == 0 {
                // 首次发送
                need_update.push((idx, 1, current.wrapping_add(seg.rto), seg.rto, false));
                sends.push((seg.sn, seg.ts, seg.una, seg.wnd, seg.data.clone()));
            } else {
                // 检查是否超时
                let diff = (current as i32).wrapping_sub(seg.resendts as i32);
                if diff >= 0 {
                    // 超时重传
                    if seg.xmit >= self.dead_link {
                        // 超过最大重传次数，连接断开
                        return Err(KcpError::IoError(
                            "Connection lost due to too many retransmissions".to_string(),
                        ));
                    }

                    let new_rto = seg.rto * 2;
                    let clamped_rto = new_rto.min(IKCP_RTO_MAX);
                    need_update.push((
                        idx,
                        seg.xmit + 1,
                        current.wrapping_add(seg.rto),
                        clamped_rto,
                        true,
                    ));
                    sends.push((seg.sn, seg.ts, seg.una, seg.wnd, seg.data.clone()));
                }
            }

            // 快速重传
            if seg.fastack >= self.fastresend as u32 {
                // 触发快速重传
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
                    false,
                ));
                sends.push((seg.sn, seg.ts, seg.una, seg.wnd, seg.data.clone()));
            }
        }

        // 第二步：实际发送
        for (sn, ts, una, wnd, data) in sends {
            self.send_segment(IKCP_CMD_PUSH, sn, ts, una, wnd, data)?;
        }

        // 第三步：更新segment字段
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

        // 更新拥塞窗口
        if lost {
            // 丢包，减小拥塞窗口
            self.ssthresh = (self.cwnd / 2).max(IKCP_THRESH_MIN);
            self.cwnd = 1;
            self.incr = 0;
        } else if self.timediff(current, self.ts_lastack) >= 0 {
            // 正常，增加拥塞窗口
            if self.cwnd < self.ssthresh {
                // 慢启动
                self.incr += self.mss;
            } else {
                // 拥塞避免
                self.incr += if self.mss > 0 {
                    self.mss * self.mss / self.cwnd
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

    /// 发送数据段
    ///
    /// 对应C代码的 ikcp_output() 函数的发送部分
    ///
    /// # 参数
    ///
    /// - `cmd`: 命令类型
    /// - `sn`: 序列号
    /// - `ts`: 时间戳
    /// - `una`: 未确认的最小序列号
    /// - `wnd`: 窗口大小
    /// - `data`: 数据负载
    fn send_segment(
        &mut self,
        cmd: u32,
        sn: u32,
        ts: u32,
        una: u32,
        wnd: u32,
        data: Vec<u8>,
    ) -> KcpResult<()> {
        // 编码数据包（24字节头部 + 数据）
        let size = IKCP_OVERHEAD as usize + data.len();
        if size > self.buffer.len() {
            return Err(KcpError::BufferTooSmall);
        }

        let mut offset = 0;

        // 0-3: conv
        self.buffer[0..4].copy_from_slice(&self.conv.to_be_bytes());
        offset += 4;

        // 4: cmd
        self.buffer[offset] = cmd as u8;
        offset += 1;

        // 5-6: wnd
        let wnd_bytes = (wnd as u16).to_be_bytes();
        self.buffer[offset..offset + 2].copy_from_slice(&wnd_bytes);
        offset += 2;

        // 7-8: unused (保留)
        offset += 2;

        // 9-12: ts
        let ts_bytes = ts.to_be_bytes();
        self.buffer[offset..offset + 4].copy_from_slice(&ts_bytes);
        offset += 4;

        // 13-16: sn
        let sn_bytes = sn.to_be_bytes();
        self.buffer[offset..offset + 4].copy_from_slice(&sn_bytes);
        offset += 4;

        // 17-20: una
        let una_bytes = una.to_be_bytes();
        self.buffer[offset..offset + 4].copy_from_slice(&una_bytes);
        offset += 4;

        // 21-24: len
        let len_bytes = (data.len() as u32).to_be_bytes();
        self.buffer[offset..offset + 4].copy_from_slice(&len_bytes);
        offset += 4;

        // 数据部分
        if !data.is_empty() {
            self.buffer[offset..offset + data.len()].copy_from_slice(&data);
            offset += data.len();
        }

        // 准备发送的数据
        let send_data = self.buffer[..offset].to_vec();

        // 调用output回调（使用take避免借用检查器冲突）
        let output = self.output.take().ok_or(KcpError::OutputNotSet)?;
        let result = output(&send_data, self);
        self.output = Some(output); // 放回output
        result?;

        self.xmit += 1;

        Ok(())
    }

    /// 更新KCP状态
    ///
    /// 对应C代码的 ikcp_update() 函数 (ikcp.c:1024)
    ///
    /// # 参数
    ///
    /// - `current`: 当前时间戳（毫秒）
    ///
    /// # 实现要点
    ///
    /// 1. 更新current时间戳
    /// 2. 检查是否需要flush
    /// 3. 处理超时重传
    ///
    /// # 示例
    ///
    /// ```ignore
    /// // 定期调用update
    /// kcp.update(current_timestamp_ms());
    /// ```
    pub fn update(&mut self, current: u32) {
        self.current = current;

        // 首次更新
        if !self.updated {
            self.updated = true;
            self.ts_flush = self.current;
        }

        // 检查是否需要flush
        let slap = self.timediff(self.current, self.ts_flush);

        if slap >= 0 {
            // 需要flush
            self.ts_flush = self.current.wrapping_add(self.interval);

            // 调用flush
            let _ = self.flush();
        }
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
    pub fn check(&self, current: u32) -> u32 {
        let mut ts_flush = self.ts_flush;

        // 检查是否有待发送的数据
        if !self.snd_queue.is_empty() || !self.snd_buf.is_empty() {
            // 计算最早的重传时间
            for seg in self.snd_buf.iter() {
                let diff = self.timediff(seg.resendts, current);
                if diff < 0 {
                    // 已经超时，立即需要更新
                    return current;
                } else if diff < self.timediff(ts_flush, current) {
                    ts_flush = seg.resendts;
                }
            }
        }

        // 检查是否需要发送窗口探测
        if (self.probe & IKCP_ASK_SEND) != 0 {
            return current;
        }

        // 返回下次更新时间
        if self.timediff(ts_flush, current) > 0 {
            ts_flush
        } else {
            current
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kcp_new() {
        let config = KcpConfig::default();
        let kcp = Kcp::new(0x11223344, config);
        assert!(kcp.is_ok());

        let kcp = kcp.unwrap();
        assert_eq!(kcp.conv, 0x11223344);
        assert_eq!(kcp.mtu, 1400);
        assert_eq!(kcp.mss, 1376); // 1400 - 24
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
