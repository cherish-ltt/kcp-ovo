use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::mpsc::Sender;

use super::Clinet;
use super::drain::{SendCommand, drain};
use super::lifecycle::Lifecycle;
use crate::core::kcp::Kcp;
use crate::{KcpError, KcpResult};

/// 可克隆的连接发送句柄；接收和生命周期仍由 [`super::KcpStream`] 管理。
///
/// 所有句柄与 owner 共用一个有界 FIFO。多个生产者按实际成功入队顺序排列，
/// 不保证任务启动顺序。丢弃句柄不关闭连接，owner close/Drop 会使句柄失效。
#[derive(Clone)]
pub struct KcpSender {
    pub(super) channel: Sender<SendCommand>,
    pub(super) lifecycle: Arc<Lifecycle>,
    pub(super) mss: usize,
}

impl KcpSender {
    /// 等待有界队列空间并入队；成功不代表对端已接收。
    ///
    /// 超限消息在入队前拒绝。取消未入队的 future 不发送数据；已经入队的
    /// 成功与 close 竞争时仍可保留成功结果，但立即关闭可能丢弃数据。
    pub fn send(
        &self,
        data: &[u8],
    ) -> impl Future<Output = KcpResult<()>> + Send + 'static + use<> {
        let request = self.prepare(data);
        async move {
            let (sender, data) = request?;
            sender.enqueue(SendCommand::Data(data)).await
        }
    }

    /// 尝试入队；队满或连接终止时明确报错。
    pub fn try_send(&self, data: &[u8]) -> KcpResult<()> {
        let (sender, data) = self.prepare(data)?;
        sender.lifecycle.with_open(|| {
            sender
                .channel
                .try_send(SendCommand::Data(data))
                .map_err(|error| KcpError::IoError(format!("stream try send err:{error}")))
        })?
    }

    /// 等待屏障前成功入队的数据获得 KCP 确认，并输出待发 ACK/窗口通告。
    ///
    /// 总期限包含入队时间；尚未入队的并发发送及屏障后发送不延长该等待。
    /// 取消和超时不撤回数据，成功不代表对端应用已经读取或持久化。
    pub fn drain(
        &self,
        wait: Duration,
    ) -> impl Future<Output = KcpResult<()>> + Send + 'static + use<> {
        drain(Ok(self.clone()), wait)
    }

    fn prepare(&self, data: &[u8]) -> KcpResult<(Self, Vec<u8>)> {
        self.lifecycle.with_open(|| ())?;
        Kcp::validate_send_size(data.len(), self.mss, false)?;
        Ok((self.clone(), data.to_vec()))
    }

    pub(super) async fn enqueue(&self, command: SendCommand) -> KcpResult<()> {
        tokio::select! {
            biased;
            () = self.lifecycle.closed() => {
                Err(KcpError::IoError("send worker stopped".to_string()))
            }
            permit = self.channel.reserve() => {
                let permit = permit.map_err(|_| KcpError::IoError("send worker stopped".to_string()))?;
                self.lifecycle.with_open(|| permit.send(command))
            }
        }
    }
}

/// 可克隆的监听器发送句柄，只能向已有会话发送。
///
/// [`super::KcpListener`] 保留唯一接收权和关闭权。句柄不创建会话，
/// 旧实例的发送或确认等待不会转投同地址重建的会话。
#[derive(Clone)]
pub struct KcpListenerSender {
    pub(super) clients: Arc<DashMap<SocketAddr, Clinet>>,
    pub(super) lifecycle: Arc<Lifecycle>,
}

impl KcpListenerSender {
    /// 调用时捕获当前会话，等待其有界 FIFO 空间后入队。
    ///
    /// 消息校验、排序、取消和关闭语义与 [`KcpSender::send`] 相同。
    pub fn send_to(
        &self,
        data: &[u8],
        addr: SocketAddr,
    ) -> impl Future<Output = KcpResult<()>> + Send + 'static + use<> {
        let request = self.target(addr).and_then(|sender| sender.prepare(data));
        async move {
            let (sender, data) = request?;
            sender.enqueue(SendCommand::Data(data)).await
        }
    }

    /// 调用时绑定当前会话的确认屏障，期限及成功语义与 [`KcpSender::drain`] 相同。
    pub fn drain_to(
        &self,
        addr: SocketAddr,
        wait: Duration,
    ) -> impl Future<Output = KcpResult<()>> + Send + 'static + use<> {
        drain(self.target(addr), wait)
    }

    fn target(&self, addr: SocketAddr) -> KcpResult<KcpSender> {
        self.lifecycle.with_open(|| ())?;
        self.clients
            .get(&addr)
            .map(|client| client.sender.clone())
            .ok_or_else(|| KcpError::IoError("session unavailable".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn close_rejects_reserved_send_and_preserves_committed_result() {
        let (channel, mut commands) = mpsc::channel(1);
        let lifecycle = Lifecycle::new();
        let sender = KcpSender { channel, lifecycle: lifecycle.clone(), mss: 1372 };
        let committed = sender.send(b"committed").await;
        let waiting = sender.send(b"waiting");
        tokio::pin!(waiting);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx))).await.is_pending()
        );
        lifecycle.close();
        assert!(committed.is_ok());
        assert!(waiting.await.is_err());
        assert!(sender.send(b"closed").await.is_err());
        assert!(sender.try_send(b"closed").is_err());
        assert!(
            matches!(commands.recv().await, Some(SendCommand::Data(data)) if data == b"committed")
        );
        assert!(commands.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancelled_queue_wait_does_not_enqueue_and_drain_preserves_committed_success()
    -> Result<(), &'static str> {
        let (channel, mut commands) = mpsc::channel(1);
        let lifecycle = Lifecycle::new();
        let sender = KcpSender { channel, lifecycle: lifecycle.clone(), mss: 1372 };
        sender.try_send(b"first").unwrap();
        let mut waiting = Box::pin(sender.send(b"cancelled"));
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx))).await.is_pending()
        );
        drop(waiting);
        assert!(matches!(commands.recv().await, Some(SendCommand::Data(_))));
        assert!(commands.try_recv().is_err());
        let waiting = sender.drain(Duration::from_secs(1));
        tokio::pin!(waiting);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx))).await.is_pending()
        );
        let Some(SendCommand::Drain(reply)) = commands.recv().await else {
            return Err("missing barrier");
        };
        lifecycle.with_open(|| reply.send(())).unwrap().unwrap();
        lifecycle.close();
        waiting.await.unwrap();
        Ok(())
    }

    #[tokio::test]
    async fn termination_wakes_drain_before_worker_drops_reply() -> Result<(), &'static str> {
        let (channel, mut commands) = mpsc::channel(1);
        let lifecycle = Lifecycle::new();
        let sender = KcpSender { channel, lifecycle: lifecycle.clone(), mss: 1372 };
        let waiting = sender.drain(Duration::from_secs(60));
        tokio::pin!(waiting);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx))).await.is_pending()
        );
        let Some(SendCommand::Drain(reply)) = commands.recv().await else {
            return Err("missing barrier");
        };
        lifecycle.close();
        assert!(waiting.await.is_err());
        assert!(lifecycle.with_open(|| reply.send(())).is_err());
        let (reply, completed) = oneshot::channel::<()>();
        assert!(lifecycle.with_open(|| reply.send(())).is_err());
        assert!(completed.await.is_err());
        Ok(())
    }
}
