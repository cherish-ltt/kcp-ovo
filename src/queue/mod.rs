//! 队列管理模块
//!
//! 本模块提供了KCP协议中使用的队列数据结构，包括数据段和双向队列

pub mod deque;
pub mod segment;

// 导出常用类型
pub use deque::KcpDeque;
pub use segment::Segment;
