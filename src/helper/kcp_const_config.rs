// KCP协议常量定义
pub const IKCP_CMD_PUSH: u32 = 81; // 推送数据
pub const IKCP_CMD_ACK: u32 = 82; // 确认
pub const IKCP_CMD_WASK: u32 = 83; // 窗口探测请求
pub const IKCP_CMD_WINS: u32 = 84; // 窗口大小通知

pub const IKCP_ASK_SEND: u32 = 1; // 需要发送窗口探测
pub const IKCP_ASK_TELL: u32 = 2; // 需要告知窗口大小

pub const IKCP_OVERHEAD: usize = 28; // 包头大小
pub const IKCP_DEADLINK: u32 = 20; // 最大重传次数（与 C 原版一致）
pub const IKCP_THRESH_INIT: u32 = 2; // cwnd 初值（C 原版同为 2 段起步）
/// 慢启动阈值的初值。
///
/// 本实现的 cwnd 初值为 2（cwnd=0 时发送准入恒不通过，首段永远发不出）。
/// 若 ssthresh 也取 2，则 `cwnd < ssthresh` 永远不成立，慢启动被整段跳过，
/// 拥塞避免分支每次只加 mss*mss/cwnd 且 incr 在整数除法下几乎不增长，
/// 最终 cwnd 长期卡在 1，吞吐退化为 1 段/RTT。
/// 这里取一个足够大的初值，让新连接先按慢启动爬升。
pub const IKCP_THRESH_INIT_WND: u32 = 64;
#[allow(dead_code)]
pub const IKCP_THRESH_MIN: u32 = 2; // 最小慢启动阈值
pub const IKCP_FASTACK_LIMIT: i32 = 5; // 快速重传限制

/// nodelay 模式下的最小 RTO（毫秒）
pub const IKCP_RTO_NDL: u64 = 30;
pub const IKCP_RTO_MIN: u64 = 100; // 最小RTO
pub const IKCP_RTO_DEF: u64 = 200; // 默认RTO
pub const IKCP_RTO_MAX: u64 = 60000; // 最大RTO

#[allow(dead_code)]
pub const IKCP_WND_SND: u32 = 32; // 默认发送窗口
pub const IKCP_WND_RCV: u32 = 128; // 默认接收窗口
#[allow(dead_code)]
pub const IKCP_MTU_DEF: u32 = 1400; // 默认MTU
#[allow(dead_code)]
pub const IKCP_INTERVAL: u64 = 100; // 默认更新间隔

/// 窗口探测初始等待时间（毫秒），与 C 原版一致
pub const IKCP_PROBE_INIT: u64 = 5000;
/// 窗口探测最大等待时间（毫秒）
pub const IKCP_PROBE_LIMIT: u64 = 120000;
