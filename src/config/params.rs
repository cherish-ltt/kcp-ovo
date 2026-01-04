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
    pub interval: u32,

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

    /// 是否流式模式
    ///
    /// true: 流式模式，不分消息边界
    /// false: 消息模式，保留消息边界
    /// 默认值：false
    pub stream: bool,

    /// 发送窗口大小
    ///
    /// 发送窗口的最大段数
    /// 默认值：32
    pub snd_wnd: u32,

    /// 接收窗口大小
    ///
    /// 接收窗口的最大段数
    /// 默认值：128
    pub rcv_wnd: u32,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            mtu: 1400,
            interval: 100,
            nodelay: false,
            fastresend: 0,
            nocwnd: false,
            stream: false,
            snd_wnd: 32,
            rcv_wnd: 128,
        }
    }
}

impl KcpConfig {
    /// 创建快速模式配置（最低延迟）
    ///
    /// 对应C代码: ikcp_nodelay(kcp, 1, 20, 2, 1)
    ///
    /// # 返回
    ///
    /// 返回一个针对低延迟优化的配置
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::config::params::KcpConfig;
    ///
    /// let config = KcpConfig::fast_mode();
    /// assert!(config.nodelay);
    /// assert_eq!(config.interval, 20);
    /// ```
    pub fn fast_mode() -> Self {
        Self {
            mtu: 1400,
            interval: 20,
            nodelay: true,
            fastresend: 2,
            nocwnd: true,
            stream: false,
            snd_wnd: 32,
            rcv_wnd: 128,
        }
    }

    /// 创建普通模式配置（默认）
    ///
    /// # 返回
    ///
    /// 返回一个平衡性能和可靠性的默认配置
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::config::params::KcpConfig;
    ///
    /// let config = KcpConfig::normal_mode();
    /// assert_eq!(config.interval, 100);
    /// ```
    pub fn normal_mode() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = KcpConfig::default();
        assert_eq!(config.mtu, 1400);
        assert_eq!(config.interval, 100);
        assert_eq!(config.nodelay, false);
        assert_eq!(config.fastresend, 0);
        assert_eq!(config.nocwnd, false);
        assert_eq!(config.stream, false);
        assert_eq!(config.snd_wnd, 32);
        assert_eq!(config.rcv_wnd, 128);
    }

    #[test]
    fn test_fast_mode() {
        let config = KcpConfig::fast_mode();
        assert_eq!(config.mtu, 1400);
        assert_eq!(config.interval, 20);
        assert_eq!(config.nodelay, true);
        assert_eq!(config.fastresend, 2);
        assert_eq!(config.nocwnd, true);
        assert_eq!(config.stream, false);
    }

    #[test]
    fn test_normal_mode() {
        let config = KcpConfig::normal_mode();
        assert_eq!(config.interval, 100);
        assert_eq!(config.nodelay, false);
    }

    #[test]
    fn test_config_clone() {
        let config1 = KcpConfig::fast_mode();
        let config2 = config1.clone();
        assert_eq!(config1.interval, config2.interval);
        assert_eq!(config1.nodelay, config2.nodelay);
    }
}
