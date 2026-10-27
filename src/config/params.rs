//! KCP配置参数
//!
//! 本模块定义了KCP协议的配置选项

/// KCP配置选项
///
/// 定义了KCP协议运行时的各种配置参数
#[derive(Debug, Clone)]
pub struct KcpConfig {
    /// 最大传输单元（字节）
    ///
    /// 默认值：1400
    /// 范围：通常在576-1500之间
    pub mtu: u32,

    /// 内部更新间隔（毫秒）
    ///
    /// KCP内部的定时更新间隔
    /// 默认值：100ms
    pub interval: u64,

    /// 是否启用无延迟模式
    ///
    /// true: 禁用纳格算法，立即发送数据
    /// false: 启用纳格算法，等待更多数据一起发送
    /// 默认值：false
    pub nodelay: bool,

    /// 快速重传模式
    ///
    /// 0: 禁用快速重传
    /// 1: 启用快速重传（1个ACK触发）
    /// 2: 快速重传（2个ACK触发）
    /// 默认值：0
    pub fastresend: i32,

    /// 是否禁用拥塞控制
    ///
    /// true: 禁用拥塞控制，发送速率不受限制
    /// false: 启用拥塞控制
    /// 默认值：false
    pub nocwnd: bool,

    /// 发送窗口大小
    ///
    /// 发送窗口的最大段数
    /// 默认值：32
    pub snd_wnd: u16,

    /// 接收窗口大小
    ///
    /// 接收窗口的最大段数
    /// 默认值：128
    pub rcv_wnd: u16,
    /// 流模式：
    ///
    /// - `false`（默认）：有消息边界，分片 frg 递减链，最多 255 分片
    /// - `true`：纯字节流语义，所有分片 frg=0，无消息边界，无分片上限
    ///
    /// 流模式与非流模式不能互连互通。
    pub stream_mode: bool,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            mtu: 1400,
            interval: 100,
            nodelay: false,
            fastresend: 0,
            nocwnd: false,
            snd_wnd: 32,
            rcv_wnd: 128,
            stream_mode: false,
        }
    }
}

/// 基础默认值，供各构造方法共享
const BASE_MTU: u32 = 1400;
const BASE_SND_WND: u16 = 32;
const BASE_RCV_WND: u16 = 128;

impl KcpConfig {
    /// 快速模式（低延迟）：nodelay, interval=20, fastresend=2, nocwnd
    pub fn fast_mode() -> Self {
        Self {
            mtu: BASE_MTU,
            interval: 20,
            nodelay: true,
            fastresend: 2,
            nocwnd: true,
            snd_wnd: BASE_SND_WND,
            rcv_wnd: BASE_RCV_WND,
            stream_mode: false,
        }
    }

    /// 普通模式（默认平衡配置）
    pub fn normal_mode() -> Self {
        Self::default()
    }

    /// 快速模式并指定发送/接收窗口的段数
    ///
    /// `fast_mode()` 的其余参数（nodelay、interval=20、fastresend=2、nocwnd）保持不变。
    ///
    /// 之所以需要这个函数：`nocwnd=true` 时发送量只受 `snd_wnd.min(rmt_wnd)` 限制，
    /// 拥塞窗口不再参与调节，`fast_mode()` 默认的 32 段就成了吞吐的硬上限。
    /// 需要在单连接上跑满带宽时应显式放大窗口。
    ///
    /// 窗口按段数计，单段载荷约 `mtu - OVERHEAD`（默认 1372 字节），
    /// 因此 `snd_wnd=512` 约对应 700KB 的在途数据。
    ///
    /// # 参数
    ///
    /// - `snd_wnd`: 发送窗口段数，须大于 0
    /// - `rcv_wnd`: 接收窗口段数，须不小于 `snd_wnd` 才能让对方窗口通告不成为瓶颈
    ///
    /// # 示例
    ///
    /// ```
    /// use kcp_ovo::config::KcpConfig;
    ///
    /// let config = KcpConfig::fast_mode_with_wnd(512, 512);
    /// assert_eq!(config.snd_wnd, 512);
    /// assert_eq!(config.rcv_wnd, 512);
    /// assert!(config.nocwnd);
    /// ```
    pub fn fast_mode_with_wnd(snd_wnd: u16, rcv_wnd: u16) -> Self {
        Self { snd_wnd, rcv_wnd, ..Self::fast_mode() }
    }

    /// 流模式（无消息边界，无 255 分片限制）
    ///
    /// 流模式下：
    /// - 所有分片 frg=0，不再通过 frg 链组装消息
    /// - 数据超出 MSS 时自动切分，无数量上限
    /// - 接收端按 sn 顺序连续拼接所有 segment 字节后一次性交付
    /// - `peeksize()` 仅返回第一个 segment 大小
    /// - 默认使用低延迟参数（nodelay=true, interval=20, fastresend=2）
    ///
    /// 注意：流模式与非流模式不能互连互通，两端的 `stream_mode` 必须一致。
    pub fn stream_mode() -> Self {
        Self {
            mtu: BASE_MTU,
            interval: 20,
            nodelay: true,
            fastresend: 2,
            nocwnd: true,
            snd_wnd: BASE_SND_WND,
            rcv_wnd: BASE_RCV_WND,
            stream_mode: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = KcpConfig::default();
        assert_eq!(c.mtu, 1400);
        assert_eq!(c.interval, 100);
        assert!(!c.nodelay && !c.nocwnd && !c.stream_mode);
        assert_eq!(c.fastresend, 0);
    }

    #[test]
    fn test_fast_mode() {
        let c = KcpConfig::fast_mode();
        assert!(c.nodelay && c.nocwnd && !c.stream_mode);
        assert_eq!(c.interval, 20);
        assert_eq!(c.fastresend, 2);
    }

    #[test]
    fn test_stream_mode() {
        let c = KcpConfig::stream_mode();
        assert!(c.stream_mode && c.nodelay && c.nocwnd);
        assert_eq!(c.interval, 20);
        assert_eq!(c.fastresend, 2);
    }

    #[test]
    fn test_fast_mode_with_wnd() {
        let c = KcpConfig::fast_mode_with_wnd(512, 512);
        assert_eq!(c.snd_wnd, 512);
        assert_eq!(c.rcv_wnd, 512);
        assert!(c.nodelay && c.nocwnd && !c.stream_mode);
        assert_eq!(c.interval, 20);
        assert_eq!(c.fastresend, 2);

        let big = KcpConfig::fast_mode_with_wnd(1024, 2048);
        assert_eq!(big.snd_wnd, 1024);
        assert_eq!(big.rcv_wnd, 2048);
    }

    #[test]
    fn test_config_clone() {
        let c1 = KcpConfig::fast_mode();
        assert_eq!(c1.clone().interval, c1.interval);
    }
}
