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
    /// 创建新的数据段，发送相关字段初始化为 0
    pub fn new(kcp_packet: KcpPacket) -> Self {
        Self { resendts: 0, rto: 0, fastack: 0, xmit: 0, kcp_packet }
    }

    pub fn new_with_header_and_data(header: KcpPacketHeader, data: &[u8]) -> KcpResult<Self> {
        Ok(Self::new(KcpPacket::from_header_and_data(header, data)?))
    }

    /// 获取段的总大小（包含头部）
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

    fn make_packet(data_len: usize, payload: &[u8]) -> KcpPacket {
        let raw: Vec<u8> = [
            vec![0, 0, 0, 1, 81, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            (data_len as u32).to_be_bytes().to_vec(),
            payload.to_vec(),
        ]
        .concat();
        KcpPacket::from_bytes(&mut Bytes::copy_from_slice(&raw)).unwrap()
    }

    #[test]
    fn test_segment_new() {
        let seg = Segment::new(make_packet(0, &[]));
        assert_eq!(seg.size(), 28);
        assert!(seg.is_empty());
        assert_eq!(seg.get_conv(), 1);
        assert_eq!(seg.get_cmd(), KcpCmd::Push);
    }

    #[test]
    fn test_segment_new_with_data() {
        let payload = vec![1, 2, 3, 4, 5, 6, 7];
        let seg = Segment::new_with_header_and_data(
            KcpPacketHeader::new(1, KcpCmd::Push, 0, 1, 0, 0, 0, payload.len() as u32),
            &payload,
        )
        .unwrap();
        assert_eq!(seg.size(), 35);
        assert_eq!(seg.get_data().to_vec(), payload);
    }
}
