//! KCP控制块核心实现
//!
//! 本模块定义了KCP协议的核心控制块结构，对应C代码中的IKCPCB

use crate::config::KcpConfig;
use crate::error::{KcpError, KcpResult};
use crate::queue::{KcpDeque, Segment};

// KCP协议常量定义
const IKCP_CMD_PUSH: u32 = 81;   // 推送数据
const IKCP_CMD_ACK: u32 = 82;    // 确认
const IKCP_CMD_WASK: u32 = 83;   // 窗口探测请求
const IKCP_CMD_WINS: u32 = 84;   // 窗口大小通知

const IKCP_ASK_SEND: u32 = 1;    // 需要发送窗口探测
const IKCP_ASK_TELL: u32 = 2;    // 需要告知窗口大小

const IKCP_OVERHEAD: u32 = 24;   // 包头大小
const IKCP_DEADLINK: u32 = 20;   // 最大重传次数
const IKCP_THRESH_INIT: u32 = 2; // 初始慢启动阈值
const IKCP_THRESH_MIN: u32 = 2;  // 最小慢启动阈值
const IKCP_FASTACK_LIMIT: i32 = 5; // 快速重传限制

const IKCP_RTO_NDL: u32 = 30;    // 无延迟最小RTO
const IKCP_RTO_MIN: u32 = 100;   // 最小RTO
const IKCP_RTO_DEF: u32 = 200;   // 默认RTO
const IKCP_RTO_MAX: u32 = 60000; // 最大RTO

const IKCP_WND_SND: u32 = 32;    // 默认发送窗口
const IKCP_WND_RCV: u32 = 128;   // 默认接收窗口
const IKCP_MTU_DEF: u32 = 1400;  // 默认MTU
const IKCP_INTERVAL: u32 = 100;  // 默认更新间隔

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
    ackblock: usize,

    // 用户数据和回调
    /// 用户指针（透传给output回调）
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
    output: Option<Box<dyn Fn(&[u8], &mut Kcp) -> KcpResult<usize> + Send + Sync>>,
    /// 日志回调函数
    writelog: Option<Box<dyn Fn(&str, &Kcp) + Send + Sync>>,
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
    fn log(&self, mask: u32, msg: &str) {
        if (mask & self.logmask) != 0 {
            if let Some(ref writelog) = self.writelog {
                writelog(msg, self);
            }
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
