use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use log::error;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc::Sender, oneshot};
use tokio::time::timeout;

use super::Clinet;
use super::lifecycle::Lifecycle;
use crate::{KcpError, KcpResult};

pub(super) struct ReceivedMessage {
    pub(super) data: Vec<u8>,
    pub(super) addr: SocketAddr,
    pub(super) valid: Arc<Lifecycle>,
}

pub(super) type UdpOutput = (Vec<u8>, SocketAddr, u64, oneshot::Sender<()>);

// 仅在监听器主任务中调用，保证实际 UDP 输出与单会话回收串行完成。
pub(super) async fn send_output(
    clients: &DashMap<SocketAddr, Clinet>,
    socket: &UdpSocket,
    (data, addr, id, sent): UdpOutput,
) {
    if !clients.get(&addr).is_some_and(|client| client.id == id) {
        return;
    }
    match timeout(Duration::from_millis(100), socket.send_to(&data, addr)).await {
        Ok(Ok(_)) => {
            let _ = sent.send(());
        },
        result => {
            error!("socket.send_to err: {:?}", result);
            clients.remove_if(&addr, |_, client| client.id == id);
        },
    }
}

pub(super) struct CloseRequest {
    addr: SocketAddr,
    target: u64,
    completed: oneshot::Sender<bool>,
}

pub(super) async fn close_session(
    sender: Sender<CloseRequest>,
    lifecycle: Arc<Lifecycle>,
    addr: SocketAddr,
    target: Option<u64>,
) -> KcpResult<bool> {
    lifecycle.with_open(|| ())?;
    let Some(target) = target else { return Ok(false) };
    let (completed, mut result) = oneshot::channel();
    tokio::select! {
        biased;
        () = lifecycle.closed() => return Err(KcpError::IoError("listener stopped".to_string())),
        permit = sender.reserve() => {
            let permit = permit.map_err(|_| KcpError::IoError("listener stopped".to_string()))?;
            lifecycle.with_open(|| permit.send(CloseRequest { addr, target, completed }))?;
        }
    }
    tokio::select! {
        biased;
        result = &mut result => result.map_err(|_| KcpError::IoError("listener stopped".to_string())),
        () = lifecycle.closed() => result.try_recv().map_err(|_| KcpError::IoError("listener stopped".to_string())),
    }
}

pub(super) fn finish_close(
    clients: &DashMap<SocketAddr, Clinet>,
    lifecycle: &Lifecycle,
    request: CloseRequest,
) {
    let _ = lifecycle.with_open(|| {
        let removed = clients
            .remove_if(&request.addr, |_, client| {
                if client.id != request.target {
                    return false;
                }
                client.received.close();
                client.sender.lifecycle.close();
                true
            })
            .is_some();
        let _ = request.completed.send(removed);
    });
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::time::Instant;

    use super::*;
    use crate::KcpConfig;
    use crate::core::kcp::Kcp;
    use crate::stream::diagnostics::Diagnostics;
    use crate::stream::drain::{SendCommand, finish_drain};
    use crate::stream::{KcpListener, KcpSender, StreamConfig};

    fn client(addr: SocketAddr, id: u64) -> (Clinet, mpsc::Receiver<SendCommand>) {
        let (data_tx, _) = mpsc::channel(1);
        let (channel, commands) = mpsc::channel(1);
        let client = Clinet {
            addr,
            main_handle: tokio::spawn(std::future::pending()),
            data_tx,
            sender: KcpSender { channel, lifecycle: Lifecycle::new(), mss: 1372 },
            received: Lifecycle::new(),
            conv: 1,
            id,
            last_active: Instant::now(),
            diagnostics: Diagnostics::new(&Kcp::new(1, KcpConfig::fast_mode()).unwrap()),
        };
        (client, commands)
    }

    #[tokio::test]
    async fn cancelled_close_only_takes_effect_after_enqueue() {
        let addr = "127.0.0.1:1".parse().unwrap();
        let clients = DashMap::new();
        let lifecycle = Lifecycle::new();
        let (tx, mut requests) = mpsc::channel(1);
        clients.insert(addr, client(addr, 1).0);
        let mut admitted = Box::pin(close_session(tx.clone(), lifecycle.clone(), addr, Some(1)));
        assert!(poll_fn(|cx| Poll::Ready(admitted.as_mut().poll(cx))).await.is_pending());
        let mut waiting = Box::pin(close_session(tx.clone(), lifecycle.clone(), addr, Some(1)));
        assert!(poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx))).await.is_pending());
        drop(waiting);
        let request = requests.try_recv().unwrap();
        finish_close(&clients, &lifecycle, request);
        assert!(admitted.await.unwrap());
        assert!(requests.try_recv().is_err());

        clients.insert(addr, client(addr, 2).0);
        let mut cancelled = Box::pin(close_session(tx, lifecycle.clone(), addr, Some(2)));
        assert!(poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx))).await.is_pending());
        drop(cancelled);
        finish_close(&clients, &lifecycle, requests.try_recv().unwrap());
        assert!(clients.is_empty());
    }

    #[tokio::test]
    async fn stopped_listener_wakes_queued_and_unqueued_requests() {
        let addr = "127.0.0.1:1".parse().unwrap();
        let lifecycle = Lifecycle::new();
        let (tx, mut requests) = mpsc::channel(1);
        let mut admitted = Box::pin(close_session(tx.clone(), lifecycle.clone(), addr, Some(1)));
        assert!(poll_fn(|cx| Poll::Ready(admitted.as_mut().poll(cx))).await.is_pending());
        let mut waiting = Box::pin(close_session(tx, lifecycle.clone(), addr, Some(1)));
        assert!(poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx))).await.is_pending());
        lifecycle.close();
        assert!(admitted.await.is_err());
        assert!(waiting.await.is_err());
        let request = requests.try_recv().unwrap();
        assert!(request.completed.is_closed());
        assert!(requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn close_and_drain_committed_results_survive_delayed_observation()
    -> Result<(), &'static str> {
        let addr = "127.0.0.1:1".parse().unwrap();
        let kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        for committed in [false, true] {
            let clients = DashMap::new();
            let (session, mut commands) = client(addr, 1);
            let session_lifetime = session.sender.lifecycle.clone();
            let mut drain = Box::pin(session.sender.drain(Duration::from_secs(60)));
            assert!(poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx))).await.is_pending());
            let Some(SendCommand::Drain(reply)) = commands.recv().await else {
                return Err("missing drain barrier");
            };
            let mut pending = Some(reply);
            clients.insert(addr, session);
            if committed {
                finish_drain(&kcp, &mut pending, &session_lifetime);
            }
            let lifecycle = Lifecycle::new();
            let (tx, mut requests) = mpsc::channel(1);
            let mut close = Box::pin(close_session(tx, lifecycle.clone(), addr, Some(1)));
            assert!(poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx))).await.is_pending());
            finish_close(&clients, &lifecycle, requests.try_recv().unwrap());
            lifecycle.close();
            assert!(close.await.unwrap());
            finish_drain(&kcp, &mut pending, &session_lifetime);
            assert_eq!(drain.await.is_ok(), committed);
        }
        Ok(())
    }

    #[tokio::test]
    async fn close_discards_only_uncommitted_receive_and_reclaims_history() {
        let addr = "127.0.0.1:1".parse().unwrap();
        let clients = Arc::new(DashMap::new());
        let lifecycle = Lifecycle::new();
        let (close_tx, mut requests) = mpsc::channel(1);
        let (messages, recv_rx) = mpsc::channel(2);
        let mut listener = KcpListener {
            clients: clients.clone(),
            main_handle: tokio::spawn(std::future::pending()),
            config: StreamConfig::default(),
            recv_rx,
            close_tx,
            local_addr: addr,
            lifecycle: lifecycle.clone(),
        };
        for id in 0..1000 {
            let (session, _) = client(addr, id);
            let previous = Arc::downgrade(&session.received);
            messages
                .send(ReceivedMessage {
                    data: id.to_le_bytes().to_vec(),
                    addr,
                    valid: session.received.clone(),
                })
                .await
                .unwrap();
            clients.insert(addr, session);
            let received = if id % 2 == 0 { Some(listener.recv().await.unwrap()) } else { None };
            let mut close = Box::pin(listener.close_session(addr));
            let request = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::select! {
                    result = &mut close => panic!("close completed before execution: {result:?}"),
                    request = requests.recv() => request.unwrap(),
                }
            })
            .await
            .unwrap();
            finish_close(&clients, &lifecycle, request);
            assert!(close.await.unwrap());
            if let Some((data, _)) = received {
                assert_eq!(data, id.to_le_bytes());
            } else {
                let (next, _) = client(addr, id + 1);
                messages
                    .send(ReceivedMessage {
                        data: b"next".to_vec(),
                        addr,
                        valid: next.received.clone(),
                    })
                    .await
                    .unwrap();
                clients.insert(addr, next);
                assert_eq!(listener.recv().await.unwrap().0, b"next");
                clients.clear();
            }
            assert!(previous.upgrade().is_none(), "closed instance retained historical state");
            assert!(clients.is_empty());
        }
    }

    #[tokio::test]
    async fn queued_udp_output_is_discarded_after_close_and_address_reuse() {
        timeout(Duration::from_secs(2), async {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let addr = peer.local_addr().unwrap();
            let clients = DashMap::new();
            clients.insert(addr, client(addr, 1).0);
            let (output, mut queued) = mpsc::channel(2);
            let (sent, completed) = oneshot::channel();
            send_output(&clients, &socket, (b"before close".to_vec(), addr, 1, sent)).await;
            completed.await.unwrap();
            let mut packet = [0; 1500];
            let size = peer.recv(&mut packet).await.unwrap();
            assert_eq!(&packet[..size], b"before close");

            let (stale, old_completion) = oneshot::channel();
            output.send((b"stale output".to_vec(), addr, 1, stale)).await.unwrap();
            let lifecycle = Lifecycle::new();
            let (completed, closed) = oneshot::channel();
            finish_close(&clients, &lifecycle, CloseRequest { addr, target: 1, completed });
            assert!(closed.await.unwrap());
            clients.insert(addr, client(addr, 2).0);
            let (current, current_completion) = oneshot::channel();
            output.send((b"current output".to_vec(), addr, 2, current)).await.unwrap();
            while let Ok(output) = queued.try_recv() {
                send_output(&clients, &socket, output).await;
            }
            assert!(old_completion.await.is_err());
            current_completion.await.unwrap();
            let size = peer.recv(&mut packet).await.unwrap();
            assert_eq!(&packet[..size], b"current output");
            assert!(matches!(peer.try_recv(&mut packet), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        })
        .await
        .unwrap();
    }
}
