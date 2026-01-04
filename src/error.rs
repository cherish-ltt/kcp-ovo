//! KCP错误类型定义
//!
//! 本模块定义了KCP协议实现中使用的所有错误类型。

use std::fmt;

/// KCP错误类型
///
/// 定义了KCP协议实现中可能出现的各种错误情况
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KcpError {
    /// 无效的命令类型
    ///
    /// 当接收到未知或无效的KCP命令时返回此错误
    InvalidCommand(u32),

    /// 缓冲区太小
    ///
    /// 当提供的缓冲区不足以容纳数据时返回此错误
    BufferTooSmall,

    /// 队列为空
    ///
    /// 当尝试从空队列中读取数据时返回此错误
    QueueEmpty,

    /// 数据不完整
    ///
    /// 当接收到的数据不完整或被截断时返回此错误
    IncompleteData,

    /// 序列号错误
    ///
    /// 当序列号校验失败或不符合预期时返回此错误
    InvalidSequence,

    /// 无效的配置参数
    ///
    /// 当KCP配置参数不合法时返回此错误
    InvalidConfig(String),

    /// 输出回调未设置
    ///
    /// 当尝试发送数据但未设置输出回调函数时返回此错误
    OutputNotSet,

    /// IO错误
    ///
    /// 当发生IO相关错误时返回此错误
    IoError(String),
}

impl KcpError {
    /// 获取错误类型（类似io::Error::kind）
    pub fn kind(&self) -> KcpErrorKind {
        match self {
            KcpError::InvalidCommand(_) => KcpErrorKind::InvalidCommand,
            KcpError::BufferTooSmall => KcpErrorKind::BufferTooSmall,
            KcpError::QueueEmpty => KcpErrorKind::QueueEmpty,
            KcpError::IncompleteData => KcpErrorKind::IncompleteData,
            KcpError::InvalidSequence => KcpErrorKind::InvalidSequence,
            KcpError::InvalidConfig(_) => KcpErrorKind::InvalidConfig,
            KcpError::OutputNotSet => KcpErrorKind::OutputNotSet,
            KcpError::IoError(_) => KcpErrorKind::IoError,
        }
    }
}

/// KCP错误类型分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KcpErrorKind {
    InvalidCommand,
    BufferTooSmall,
    QueueEmpty,
    IncompleteData,
    InvalidSequence,
    InvalidConfig,
    OutputNotSet,
    IoError,
}

impl fmt::Display for KcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KcpError::InvalidCommand(cmd) => write!(f, "无效的命令类型: {}", cmd),
            KcpError::BufferTooSmall => write!(f, "缓冲区太小"),
            KcpError::QueueEmpty => write!(f, "队列为空"),
            KcpError::IncompleteData => write!(f, "数据不完整"),
            KcpError::InvalidSequence => write!(f, "序列号错误"),
            KcpError::InvalidConfig(msg) => write!(f, "无效的配置参数: {}", msg),
            KcpError::OutputNotSet => write!(f, "输出回调未设置"),
            KcpError::IoError(msg) => write!(f, "IO错误: {}", msg),
        }
    }
}

impl std::error::Error for KcpError {}

/// 从std::io::Error转换
impl From<std::io::Error> for KcpError {
    fn from(err: std::io::Error) -> Self {
        KcpError::IoError(err.to_string())
    }
}

/// KCP Result类型别名
///
/// 用于KCP协议中所有可能返回错误的函数
///
/// # 示例
///
/// ```ignore
/// use kcp_ovo::KcpResult;
///
/// fn do_something() -> KcpResult<()> {
///     // ...
///     Ok(())
/// }
/// ```
pub type KcpResult<T> = Result<T, KcpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = KcpError::InvalidCommand(99);
        assert_eq!(format!("{}", err), "无效的命令类型: 99");

        let err = KcpError::BufferTooSmall;
        assert_eq!(format!("{}", err), "缓冲区太小");

        let err = KcpError::InvalidConfig("test error".to_string());
        assert_eq!(format!("{}", err), "无效的配置参数: test error");
    }

    #[test]
    fn test_result_type() {
        let ok_result: KcpResult<()> = Ok(());
        assert!(ok_result.is_ok());

        let err_result: KcpResult<()> = Err(KcpError::QueueEmpty);
        assert!(err_result.is_err());
    }
}
