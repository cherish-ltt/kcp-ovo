use crate::{KcpError, KcpResult};

/// KCP协议命令类型
///
/// 定义了KCP协议支持的4种命令类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KcpCmd {
    /// 推送数据命令
    Push = 81,
    /// 确认命令
    Ack = 82,
    /// 窗口探测请求
    Wask = 83,
    /// 窗口大小通知
    Wins = 84,
}

impl TryFrom<u8> for KcpCmd {
    type Error = KcpError;

    fn try_from(value: u8) -> KcpResult<Self> {
        match value {
            81 => Ok(KcpCmd::Push),
            82 => Ok(KcpCmd::Ack),
            83 => Ok(KcpCmd::Wask),
            84 => Ok(KcpCmd::Wins),
            _ => Err(KcpError::InvalidCommand(value as u32)),
        }
    }
}

impl From<KcpCmd> for u32 {
    fn from(cmd: KcpCmd) -> Self {
        match cmd {
            KcpCmd::Push => 81,
            KcpCmd::Ack => 82,
            KcpCmd::Wask => 83,
            KcpCmd::Wins => 84,
        }
    }
}

impl From<KcpCmd> for u8 {
    fn from(cmd: KcpCmd) -> Self {
        match cmd {
            KcpCmd::Push => 81,
            KcpCmd::Ack => 82,
            KcpCmd::Wask => 83,
            KcpCmd::Wins => 84,
        }
    }
}
