//! 大端序编解码工具
//!
//! 本模块提供了大端序整数的编解码功能，对应C代码中的ikcp_encode/decode函数

use crate::error::{KcpError, KcpResult};

/// 大端序编解码器
///
/// 提供大端序（Big-Endian）的8位、16位、32位无符号整数的编解码功能
pub struct Encoder;

impl Encoder {
    /// 编码8位无符号整数
    ///
    /// # 参数
    ///
    /// - `buf`: 输出缓冲区
    /// - `offset`: 写入位置的偏移量
    /// - `value`: 要编码的值
    ///
    /// # 返回
    ///
    /// 返回下一个可写位置的偏移量
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::codec::encoder::Encoder;
    ///
    /// let mut buf = [0u8; 1];
    /// let next = Encoder::encode_u8(&mut buf, 0, 42);
    /// assert_eq!(buf[0], 42);
    /// assert_eq!(next, 1);
    /// ```
    #[inline]
    pub fn encode_u8(buf: &mut [u8], offset: usize, value: u8) -> usize {
        buf[offset] = value;
        offset + 1
    }

    /// 解码8位无符号整数
    ///
    /// # 参数
    ///
    /// - `buf`: 输入缓冲区
    /// - `offset`: 读取位置的偏移量
    ///
    /// # 返回
    ///
    /// 返回解码的值和下一个可读位置的偏移量
    ///
    /// # 错误
    ///
    /// 当offset超出缓冲区范围时返回KcpError::IncompleteData
    #[inline]
    pub fn decode_u8(buf: &[u8], offset: usize) -> KcpResult<(u8, usize)> {
        if offset >= buf.len() {
            return Err(KcpError::IncompleteData);
        }
        Ok((buf[offset], offset + 1))
    }

    /// 编码16位无符号整数（大端序）
    ///
    /// # 参数
    ///
    /// - `buf`: 输出缓冲区
    /// - `offset`: 写入位置的偏移量
    /// - `value`: 要编码的值
    ///
    /// # 返回
    ///
    /// 返回下一个可写位置的偏移量
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::codec::encoder::Encoder;
    ///
    /// let mut buf = [0u8; 2];
    /// Encoder::encode_u16(&mut buf, 0, 0x1234);
    /// assert_eq!(buf[0], 0x12);
    /// assert_eq!(buf[1], 0x34);
    /// ```
    #[inline]
    pub fn encode_u16(buf: &mut [u8], offset: usize, value: u16) -> usize {
        buf[offset] = (value >> 8) as u8;
        buf[offset + 1] = value as u8;
        offset + 2
    }

    /// 解码16位无符号整数（大端序）
    ///
    /// # 参数
    ///
    /// - `buf`: 输入缓冲区
    /// - `offset`: 读取位置的偏移量
    ///
    /// # 返回
    ///
    /// 返回解码的值和下一个可读位置的偏移量
    ///
    /// # 错误
    ///
    /// 当offset + 1超出缓冲区范围时返回KcpError::IncompleteData
    #[inline]
    pub fn decode_u16(buf: &[u8], offset: usize) -> KcpResult<(u16, usize)> {
        if offset + 1 >= buf.len() {
            return Err(KcpError::IncompleteData);
        }
        let value = ((buf[offset] as u16) << 8) | (buf[offset + 1] as u16);
        Ok((value, offset + 2))
    }

    /// 编码32位无符号整数（大端序）
    ///
    /// # 参数
    ///
    /// - `buf`: 输出缓冲区
    /// - `offset`: 写入位置的偏移量
    /// - `value`: 要编码的值
    ///
    /// # 返回
    ///
    /// 返回下一个可写位置的偏移量
    ///
    /// # 示例
    ///
    /// ```ignore
    /// use kcp_ovo::codec::encoder::Encoder;
    ///
    /// let mut buf = [0u8; 4];
    /// Encoder::encode_u32(&mut buf, 0, 0x12345678);
    /// assert_eq!(buf[0], 0x12);
    /// assert_eq!(buf[1], 0x34);
    /// assert_eq!(buf[2], 0x56);
    /// assert_eq!(buf[3], 0x78);
    /// ```
    #[inline]
    pub fn encode_u32(buf: &mut [u8], offset: usize, value: u32) -> usize {
        buf[offset] = (value >> 24) as u8;
        buf[offset + 1] = (value >> 16) as u8;
        buf[offset + 2] = (value >> 8) as u8;
        buf[offset + 3] = value as u8;
        offset + 4
    }

    /// 解码32位无符号整数（大端序）
    ///
    /// # 参数
    ///
    /// - `buf`: 输入缓冲区
    /// - `offset`: 读取位置的偏移量
    ///
    /// # 返回
    ///
    /// 返回解码的值和下一个可读位置的偏移量
    ///
    /// # 错误
    ///
    /// 当offset + 3超出缓冲区范围时返回KcpError::IncompleteData
    #[inline]
    pub fn decode_u32(buf: &[u8], offset: usize) -> KcpResult<(u32, usize)> {
        if offset + 3 >= buf.len() {
            return Err(KcpError::IncompleteData);
        }
        let value = ((buf[offset] as u32) << 24)
            | ((buf[offset + 1] as u32) << 16)
            | ((buf[offset + 2] as u32) << 8)
            | (buf[offset + 3] as u32);
        Ok((value, offset + 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_u8() {
        let mut buf = [0u8; 1];
        let next = Encoder::encode_u8(&mut buf, 0, 42);
        assert_eq!(next, 1);
        assert_eq!(buf[0], 42);

        let (value, next) = Encoder::decode_u8(&buf, 0).unwrap();
        assert_eq!(value, 42);
        assert_eq!(next, 1);
    }

    #[test]
    fn test_encode_decode_u16() {
        let mut buf = [0u8; 2];
        Encoder::encode_u16(&mut buf, 0, 0x1234);
        assert_eq!(buf[0], 0x12);
        assert_eq!(buf[1], 0x34);

        let (value, next) = Encoder::decode_u16(&buf, 0).unwrap();
        assert_eq!(value, 0x1234);
        assert_eq!(next, 2);
    }

    #[test]
    fn test_encode_decode_u32() {
        let mut buf = [0u8; 4];
        Encoder::encode_u32(&mut buf, 0, 0x12345678);
        assert_eq!(buf[0], 0x12);
        assert_eq!(buf[1], 0x34);
        assert_eq!(buf[2], 0x56);
        assert_eq!(buf[3], 0x78);

        let (value, next) = Encoder::decode_u32(&buf, 0).unwrap();
        assert_eq!(value, 0x12345678);
        assert_eq!(next, 4);
    }

    #[test]
    fn test_encode_decode_roundtrip_u32() {
        let test_values: Vec<u32> = vec![0, 1, 255, 256, 65535, 65536, 0xFFFFFFFF, 0x12345678];

        for value in test_values {
            let mut buf = [0u8; 4];
            Encoder::encode_u32(&mut buf, 0, value);
            let (decoded, _) = Encoder::decode_u32(&buf, 0).unwrap();
            assert_eq!(value, decoded, "Roundtrip failed for 0x{:08X}", value);
        }
    }

    #[test]
    fn test_decode_incomplete_data() {
        let buf = [0u8; 1];

        // 测试u16解码（需要2字节，但只有1字节）
        assert!(Encoder::decode_u16(&buf, 0).is_err());

        // 测试u32解码（需要4字节，但只有1字节）
        assert!(Encoder::decode_u32(&buf, 0).is_err());
    }
}
