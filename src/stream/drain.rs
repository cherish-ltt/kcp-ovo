use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::core::kcp::Kcp;
use crate::{KcpError, KcpResult};

use super::lifecycle::Lifecycle;
use super::sender::KcpSender;

pub(super) enum SendCommand {
    Data(Vec<u8>),
    Drain(oneshot::Sender<()>),
}

pub(super) async fn drain(sender: KcpResult<KcpSender>, wait: Duration) -> KcpResult<()> {
    let sender = sender?;
    let (reply, mut completed) = oneshot::channel();
    timeout(wait, async {
        sender.enqueue(SendCommand::Drain(reply)).await?;
        tokio::select! {
            biased;
            result = &mut completed => result.map_err(|_| KcpError::IoError("send worker stopped".to_string())),
            () = sender.lifecycle.closed() => completed.try_recv().map_err(|_| KcpError::IoError("send worker stopped".to_string())),
        }
    })
    .await
    .map_err(|_| KcpError::TimedOut)?
}

pub(super) fn finish_drain(
    kcp: &Kcp,
    pending: &mut Option<oneshot::Sender<()>>,
    lifecycle: &Lifecycle,
) {
    if pending.as_ref().is_some_and(|reply| reply.is_closed() || kcp.is_drained()) {
        let reply = pending.take().unwrap();
        let _ = lifecycle.with_open(|| reply.send(()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KcpConfig;
    use crate::helper::{KcpCmd, KcpPacketHeader};
    use tokio::sync::mpsc;

    #[tokio::test(start_paused = true)]
    async fn deadline_includes_full_command_queue_and_cancellation_leaves_no_request() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(SendCommand::Data(vec![1])).await.unwrap();
        let sender = KcpSender { channel: tx.clone(), lifecycle: Lifecycle::new(), mss: 1372 };
        assert_eq!(
            drain(Ok(sender.clone()), Duration::from_secs(1)).await,
            Err(KcpError::TimedOut)
        );
        assert!(matches!(rx.recv().await, Some(SendCommand::Data(_))));
        assert!(rx.try_recv().is_err());

        let waiting = tokio::spawn(drain(Ok(sender), Duration::from_secs(1)));
        let mut reply = match rx.recv().await {
            Some(SendCommand::Drain(reply)) => reply,
            _ => panic!("expected a drain request"),
        };
        assert_eq!(waiting.await.unwrap(), Err(KcpError::TimedOut));
        reply.closed().await;
    }

    #[tokio::test]
    async fn stopping_worker_wakes_pending_drain() {
        let (tx, mut rx) = mpsc::channel(1);
        let sender = KcpSender { channel: tx, lifecycle: Lifecycle::new(), mss: 1372 };
        let waiting = tokio::spawn(drain(Ok(sender), Duration::from_secs(60)));
        let request = rx.recv().await.unwrap();
        drop(request);
        assert!(matches!(waiting.await.unwrap(), Err(KcpError::IoError(_))));
    }

    #[tokio::test]
    async fn barrier_waits_for_wrapped_data_and_ack_output() {
        let mut sender = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        let mut peer = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        sender.snd_nxt = u32::MAX;
        sender.snd_una = u32::MAX;
        peer.rcv_nxt = u32::MAX;
        let (out, mut packets) = mpsc::unbounded_channel();
        sender.set_output(move |data| {
            let out = out.clone();
            async move {
                let len = data.len();
                out.send(data).unwrap();
                Ok(len)
            }
        });
        let (out, mut acks) = mpsc::unbounded_channel();
        peer.set_output(move |data| {
            let out = out.clone();
            async move {
                let len = data.len();
                out.send(data).unwrap();
                Ok(len)
            }
        });
        sender.send(b"wrapped").unwrap();
        let (reply, mut done) = oneshot::channel();
        let mut pending = Some(reply);
        let lifecycle = Lifecycle::new();
        finish_drain(&sender, &mut pending, &lifecycle);
        assert!(done.try_recv().is_err());
        sender.update(0).await.unwrap();
        while let Ok(packet) = packets.try_recv() {
            peer.input(&packet).unwrap();
        }
        assert_eq!(&peer.recv().unwrap()[..], b"wrapped");
        assert!(!peer.is_drained(), "unflushed ACK must prevent completion");
        peer.update(0).await.unwrap();
        while let Ok(packet) = acks.try_recv() {
            sender.input(&packet).unwrap();
        }
        assert_eq!(sender.snd_una, 0);
        finish_drain(&sender, &mut pending, &lifecycle);
        done.await.unwrap();
        assert!(peer.is_drained());
    }

    #[tokio::test]
    async fn barrier_waits_for_cumulative_confirmation_after_receive_backpressure() {
        let config = KcpConfig { rcv_wnd: 1, ..KcpConfig::fast_mode() };
        let mut receiver = Kcp::new(1, config).unwrap();
        let (out, mut packets) = mpsc::unbounded_channel();
        receiver.set_output(move |data| {
            let out = out.clone();
            async move {
                let len = data.len();
                out.send(data).unwrap();
                Ok(len)
            }
        });
        for sn in 0..2 {
            let mut packet =
                KcpPacketHeader::new(1, KcpCmd::Push, 0, 128, 0, sn, 0, 1).to_bytes().unwrap();
            packet.extend_from_slice(&[sn as u8]);
            receiver.input(&packet).unwrap();
        }
        receiver.update(0).await.unwrap();
        for sn in 0..2 {
            let (header, _) =
                KcpPacketHeader::from_bytes(&mut packets.try_recv().unwrap().as_slice()).unwrap();
            assert_eq!(header.cmd, KcpCmd::Ack);
            assert_eq!(header.sn, sn);
            assert_eq!(header.una, 1);
        }
        assert!(packets.try_recv().is_err());
        for byte in 0..2 {
            assert_eq!(receiver.recv().unwrap().as_slice(), &[byte]);
        }
        assert_eq!(receiver.rcv_nxt, 2);
        let (reply, mut completed) = oneshot::channel();
        let mut pending = Some(reply);
        let lifecycle = Lifecycle::new();
        finish_drain(&receiver, &mut pending, &lifecycle);
        assert!(
            matches!(completed.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "latest cumulative confirmation has not been output"
        );

        receiver.update(20).await.unwrap();
        let (header, _) =
            KcpPacketHeader::from_bytes(&mut packets.try_recv().unwrap().as_slice()).unwrap();
        assert_eq!(header.cmd, KcpCmd::Wins);
        assert_eq!(header.una, 2);
        finish_drain(&receiver, &mut pending, &lifecycle);
        completed.await.unwrap();
    }
}
