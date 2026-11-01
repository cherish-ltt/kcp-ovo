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
//! 高层异步消息 API 的完整示例见仓库 `examples/echo-server.rs` 和
//! `examples/echo-client.rs`。底层控制块位于 `kcp_ovo::core::kcp::Kcp`。

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
