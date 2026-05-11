//! # KCP - 快速可靠ARQ协议的Rust实现
//!
//! 这是一个纯Rust实现的KCP协议，完整复刻了原版C代码的功能。
//! KCP是一个低延迟、高可靠性的传输层协议，相比TCP可以降低30%-40%的延迟。
//!
//! ## 特性
//!
//! - 纯Rust实现，无FFI依赖
//! - 使用mimalloc全局分配器优化内存性能
//! - 类型安全，内存安全
//! - 详细的中文文档注释
//! - 完整的KCP协议功能
//!
//! ## 快速开始
//!
//! ```ignore
//! use kcp_ovo::{Kcp, KcpConfig};
//!
//! // 创建KCP实例
//! let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;
//!
//! // 设置输出回调
//! kcp.set_output(|data, kcp| {
//!     // 通过UDP发送数据
//!     udp_socket.send_to(data, remote_addr)?;
//!     Ok(data.len())
//! });
//!
//! // 发送数据
//! kcp.send(b"Hello, KCP!")?;
//! ```

// 使用mimalloc作为全局内存分配器
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// 模块声明
pub mod config;
pub mod core;
pub mod error;
pub mod helper;
pub mod queue;

// Stream API (feature-gated)
#[cfg(feature = "stream")]
pub mod stream;

// 其他模块将在后续添加
// pub mod reliability;
// pub mod congestion;

// 导出公共API
pub use crate::config::KcpConfig;
// pub use crate::core::Kcp;
pub use crate::error::{KcpError, KcpResult};
pub use crate::helper::KcpCmd;
pub use crate::queue::{KcpDeque, Segment};
pub use bytes;
pub use tokio;

// 导出Stream API (当stream feature启用时)
// #[cfg(feature = "stream")]
// pub use crate::stream::{KcpListener, KcpStream, StreamConfig};
