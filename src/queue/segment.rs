//! KCP数据段定义
//!
//! 本模块定义了KCP协议中的数据段结构，对应C代码中的IKCPSEG

use bytes::Bytes;

use crate::{
    KcpCmd, KcpResult,
    helper::{IKCP_OVERHEAD, KcpPacket, KcpPacketHeader},
};

/// KCP数据段
///
/// 表示KCP协议中的一个数据包段，包含头部信息和数据载荷
#[derive(Debug, Clone)]
pub struct Segment {
    /// 数据包
    pub kcp_packet: KcpPacket,

    /// 重传时间戳
    pub resendts: u64,

    /// 超时重传时间（毫秒）
    pub rto: u64,

    /// 快速重传计数
    pub fastack: u32,

    /// 发送次数
    pub xmit: u32,
}

impl Segment {
    /// 创建新的数据段
    ///
    /// # 参数
    ///
    /// - `kcp_packet`: KcpPacket
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
    /// let data: Vec<u8> = vec![0, 0, 0, 1, 81, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    /// let kcp_packet = KcpPacket::from_bytes(&mut Bytes::copy_from_slice(&data)).unwrap();
    /// let seg = Segment::new(kcp_packet);
    /// ```
    pub fn new(kcp_packet: KcpPacket) -> Self {
        Self {
            resendts: 0,
            rto: 0,
            fastack: 0,
            xmit: 0,
            kcp_packet,
        }
    }

    pub fn new_with_header_and_data(header: KcpPacketHeader, data: &[u8]) -> KcpResult<Self> {
        Ok(Self {
            resendts: 0,
            rto: 0,
            fastack: 0,
            xmit: 0,
            kcp_packet: KcpPacket::from_header_and_data(header, data)?,
        })
    }

    /// 获取段的总大小（包含头部）
    ///
    /// # 返回
    ///
    /// 返回段的总字节数，包含头部和数据载荷
    pub fn size(&self) -> usize {
        IKCP_OVERHEAD + self.kcp_packet.data.len()
    }

    /// 检查段是否为空（无数据载荷）
    pub fn is_empty(&self) -> bool {
        self.kcp_packet.data.is_empty()
    }

    /// 取数data
    pub fn get_data(&self) -> Bytes {
        self.kcp_packet.data.clone()
    }

    /// 取conv
    pub fn get_conv(&self) -> u32 {
        self.kcp_packet.header.conv
    }

    /// 取cmd
    pub fn get_cmd(&self) -> KcpCmd {
        self.kcp_packet.header.cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_segment_new() {
        let data: Vec<u8> = vec![
            0, 0, 0, 1, 81, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let kcp_packet = KcpPacket::from_bytes(&mut Bytes::copy_from_slice(&data)).unwrap();
        let seg = Segment::new(kcp_packet);
        assert_eq!(seg.size(), 28 + 0);
        assert_eq!(seg.get_data().to_vec().len(), 0);
        assert_eq!(seg.is_empty(), true);
        assert_eq!(seg.get_conv(), 1);
        assert_eq!(seg.get_cmd(), KcpCmd::Push);
    }

    #[test]
    fn test_segment_new_with_data() {
        let data: Vec<u8> = vec![
            0, 0, 0, 1, 81, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 1,
            2, 3, 4, 5, 6, 7,
        ];
        let data_payload: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7];
        let kcp_packet = KcpPacket::from_bytes(&mut Bytes::copy_from_slice(&data)).unwrap();
        let seg = Segment::new(kcp_packet);
        assert_eq!(seg.size(), 28 + 7);
        assert_eq!(seg.get_data().to_vec().len(), 7);
        assert_eq!(seg.get_data().to_vec(), data_payload);
    }
}
