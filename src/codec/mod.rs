//! 编解码模块
//!
//! 本模块提供了KCP协议数据包的编解码功能

pub mod encoder;

// 导出常用类型
pub use encoder::Encoder;
