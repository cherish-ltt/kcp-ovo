// KCP协议常量定义
pub const IKCP_CMD_PUSH: u32 = 81; // 推送数据
pub const IKCP_CMD_ACK: u32 = 82; // 确认
pub const IKCP_CMD_WASK: u32 = 83; // 窗口探测请求
pub const IKCP_CMD_WINS: u32 = 84; // 窗口大小通知

pub const IKCP_ASK_SEND: u32 = 1; // 需要发送窗口探测
pub const IKCP_ASK_TELL: u32 = 2; // 需要告知窗口大小

pub const IKCP_OVERHEAD: usize = 28; // 包头大小
pub const IKCP_DEADLINK: u32 = 100; // 最大重传次数
pub const IKCP_THRESH_INIT: u32 = 2; // 初始慢启动阈值
#[allow(dead_code)]
pub const IKCP_THRESH_MIN: u32 = 2; // 最小慢启动阈值
pub const IKCP_FASTACK_LIMIT: i32 = 5; // 快速重传限制

#[allow(dead_code)]
pub const IKCP_RTO_NDL: u64 = 30; // 无延迟最小RTO
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
