//! KCP数据段定义
//!
//! 本模块定义了KCP协议中的数据段结构，对应C代码中的IKCPSEG

/// KCP数据段
///
/// 表示KCP协议中的一个数据包段，包含头部信息和数据载荷
#[derive(Debug, Clone)]
pub struct Segment {
    /// 会话ID（连接标识符）
    pub conv: u32,

    /// 命令类型
    ///
    /// - 81: PUSH - 推送数据
    /// - 82: ACK - 确认
    /// - 83: WASK - 窗口探测请求
    /// - 84: WINS - 窗口大小通知
    pub cmd: u32,

    /// 分片索引
    ///
    /// 从0开始计数，最后一个分片的frg为0
    pub frg: u32,

    /// 可用窗口大小
    pub wnd: u32,

    /// 时间戳
    pub ts: u32,

    /// 序列号
    pub sn: u32,

    /// 未确认的最小序列号
    pub una: u32,

    /// 数据长度（字节）
    pub len: u32,

    /// 重传时间戳
    pub resendts: u32,

    /// 超时重传时间（毫秒）
    pub rto: u32,

    /// 快速重传计数
    pub fastack: u32,

    /// 发送次数
    pub xmit: u32,

    /// 数据载荷
    pub data: Vec<u8>,
}

impl Segment {
    /// 创建新的数据段
    ///
    /// # 参数
    ///
    /// - `data`: 数据载荷
    ///
    /// # 返回
    ///
    /// 返回新创建的Segment实例，所有字段初始化为默认值
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::queue::segment::Segment;
    ///
    /// let data = vec![1, 2, 3, 4, 5];
    /// let seg = Segment::new(data);
    /// assert_eq!(seg.len, 5);
    /// ```
    pub fn new(data: Vec<u8>) -> Self {
        let len = data.len() as u32;
        Self {
            conv: 0,
            cmd: 0,
            frg: 0,
            wnd: 0,
            ts: 0,
            sn: 0,
            una: 0,
            len,
            resendts: 0,
            rto: 0,
            fastack: 0,
            xmit: 0,
            data,
        }
    }

    /// 创建空的数据段
    ///
    /// # 返回
    ///
    /// 返回一个不包含数据载荷的Segment实例
    pub fn empty() -> Self {
        Self {
            conv: 0,
            cmd: 0,
            frg: 0,
            wnd: 0,
            ts: 0,
            sn: 0,
            una: 0,
            len: 0,
            resendts: 0,
            rto: 0,
            fastack: 0,
            xmit: 0,
            data: Vec::new(),
        }
    }

    /// 获取段的总大小（包含头部）
    ///
    /// # 返回
    ///
    /// 返回段的总字节数，包含24字节的头部和数据载荷
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::queue::segment::Segment;
    ///
    /// let data = vec![1, 2, 3, 4, 5];
    /// let seg = Segment::new(data);
    /// assert_eq!(seg.size(), 24 + 5); // 头部24字节 + 数据5字节
    /// ```
    pub fn size(&self) -> usize {
        24 + self.data.len() // IKCP_OVERHEAD = 24
    }

    /// 检查段是否为空（无数据载荷）
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl Default for Segment {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_new() {
        let data = vec![1, 2, 3, 4, 5];
        let seg = Segment::new(data.clone());
        assert_eq!(seg.len, 5);
        assert_eq!(seg.data, data);
        assert_eq!(seg.conv, 0);
        assert_eq!(seg.cmd, 0);
    }

    #[test]
    fn test_segment_empty() {
        let seg = Segment::empty();
        assert_eq!(seg.len, 0);
        assert!(seg.data.is_empty());
        assert!(seg.is_empty());
    }

    #[test]
    fn test_segment_size() {
        let data = vec![1, 2, 3, 4, 5];
        let seg = Segment::new(data);
        assert_eq!(seg.size(), 29); // 24字节头部 + 5字节数据
    }

    #[test]
    fn test_segment_default() {
        let seg: Segment = Default::default();
        assert!(seg.is_empty());
        assert_eq!(seg.len, 0);
    }
}
