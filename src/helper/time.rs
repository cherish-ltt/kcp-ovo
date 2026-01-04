//! 时间工具模块
//!
//! 本模块提供时间相关的辅助函数

use std::time::{SystemTime, UNIX_EPOCH};

/// 获取当前时间戳（毫秒）
///
/// # 返回
///
/// 返回从UNIX纪元开始的毫秒数
#[inline]
pub fn current_millis() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u32
}

/// 生成唯一的会话ID
///
/// # 注意
///
/// 在实际应用中,conv值应该通过握手协商而不是随机生成
/// 这里使用时间戳的简单方式仅用于示例
///
/// # 返回
///
/// 返回一个基于当前时间戳的唯一u32值
#[inline]
pub fn generate_conv() -> u32 {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    (timestamp & 0xFFFFFFFF) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_millis() {
        let ts1 = current_millis();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let ts2 = current_millis();
        assert!(ts2 > ts1);
    }

    #[test]
    fn test_generate_conv() {
        let conv1 = generate_conv();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let conv2 = generate_conv();
        assert_ne!(conv1, conv2);
    }
}
