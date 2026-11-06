#![cfg(feature = "stream")]

mod common;

use std::future::Future;
use std::time::Duration;

use kcp_ovo::KcpCmd;
use kcp_ovo::helper::KcpPacketHeader;
use kcp_ovo::stream::{CHANNEL_SIZE, KcpListener, KcpStream, StreamConfig};
use tokio::net::UdpSocket;
use tokio::time::timeout;

use common::stress::push_packet;

const LIMIT: Duration = Duration::from_secs(5);

async fn pending<Result: std::fmt::Debug>(future: impl Future<Output = Result>) {
    tokio::select! {
        biased;
        result = future => panic!("operation unexpectedly completed: {result:?}"),
        () = std::future::ready(()) => {},
    }
}

async fn receive_packet(socket: &UdpSocket, command: KcpCmd) -> KcpPacketHeader {
    timeout(LIMIT, async {
        let mut packet = [0; 1500];
        loop {
            let size = socket.recv(&mut packet).await.unwrap();
            let (header, _) = KcpPacketHeader::from_bytes(&mut &packet[..size]).unwrap();
            if header.cmd == command {
                return header;
            }
        }
    })
    .await
    .unwrap()
}

async fn peer(listener: &mut KcpListener, conv: u32) -> UdpSocket {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(&push_packet(conv, 0, b"hello"), listener.local_addr()).await.unwrap();
    let (message, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
    assert_eq!(message, b"hello");
    assert_eq!(addr, peer.local_addr().unwrap());
    receive_packet(&peer, KcpCmd::Ack).await;
    peer
}

#[tokio::test]
async fn missing_session_is_idempotent() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(!listener.close_session("127.0.0.1:1".parse().unwrap()).await.unwrap());
    let peer = peer(&mut listener, 41).await;
    let addr = peer.local_addr().unwrap();
    let earlier = listener.close_session(addr);
    assert!(listener.close_session(addr).await.unwrap());
    assert!(!earlier.await.unwrap());
    assert!(!listener.close_session(addr).await.unwrap());
    listener.close().unwrap();
    assert!(listener.close_session(addr).await.is_err());
}

#[tokio::test]
async fn close_releases_capacity_without_idle_wait() {
    let config = StreamConfig { max_sessions: 1, ..StreamConfig::default() };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
    let listening = listener.local_addr();
    let first = peer(&mut listener, 41).await;
    let second = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    second.send_to(&push_packet(42, 0, b"blocked"), listening).await.unwrap();
    first.send_to(&push_packet(41, 1, b"barrier"), listening).await.unwrap();
    assert_eq!(timeout(LIMIT, listener.recv()).await.unwrap().unwrap().0, b"barrier");
    assert!(listener.send_to(b"missing", second.local_addr().unwrap()).await.is_err());
    assert!(listener.close_session(first.local_addr().unwrap()).await.unwrap());
    second.send_to(&push_packet(42, 0, b"admitted"), listening).await.unwrap();
    assert_eq!(
        timeout(LIMIT, listener.recv()).await.unwrap().unwrap(),
        (b"admitted".to_vec(), second.local_addr().unwrap())
    );
    assert_eq!(listener.local_addr(), listening);
}

#[tokio::test]
async fn close_invalidates_backlog_and_old_operations_without_harming_replacement() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = peer(&mut listener, 41).await;
    let addr = peer.local_addr().unwrap();
    let other = self::peer(&mut listener, 51).await;
    peer.send_to(&push_packet(41, 1, b"discard me"), listener.local_addr()).await.unwrap();
    let ack = receive_packet(&peer, KcpCmd::Ack).await;
    assert_eq!(ack.una, 2);
    other.send_to(&push_packet(51, 1, b"keep me"), listener.local_addr()).await.unwrap();
    receive_packet(&other, KcpCmd::Ack).await;
    let old_close = listener.close_session(addr);
    let old_drain = listener.drain_to(addr, LIMIT);
    let old_send = listener.sender().send_to(b"must not switch instance", addr);
    assert!(listener.close_session(addr).await.unwrap());
    assert!(listener.diagnostics_for(addr).is_err());
    assert!(old_drain.await.is_err());
    assert_eq!(
        timeout(LIMIT, listener.recv()).await.unwrap().unwrap(),
        (b"keep me".to_vec(), other.local_addr().unwrap())
    );

    peer.send_to(&push_packet(42, 0, b"replacement"), listener.local_addr()).await.unwrap();
    assert_eq!(timeout(LIMIT, listener.recv()).await.unwrap().unwrap().0, b"replacement");
    assert!(!old_close.await.unwrap());
    assert!(old_send.await.is_err());
    listener.send_to(b"new reply", addr).await.unwrap();
    let header = receive_packet(&peer, KcpCmd::Push).await;
    assert_eq!(header.conv, 42);
}

#[tokio::test]
async fn backpressure_zero_window_and_blackhole_do_not_delay_close() {
    timeout(LIMIT, async {
        for zero_window in [false, true] {
            let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let peer = peer(&mut listener, 41).await;
            let addr = peer.local_addr().unwrap();
            if zero_window {
                let mut packet =
                    KcpPacketHeader::new(41, KcpCmd::Push, 0, 0, 0, 1, 0, 1).to_bytes().unwrap();
                packet.push(0);
                peer.send_to(&packet, listener.local_addr()).await.unwrap();
                assert_eq!(listener.recv().await.unwrap().0, [0]);
            }
            listener.send_to(b"unconfirmed", addr).await.unwrap();
            let drain = listener.drain_to(addr, LIMIT);
            tokio::pin!(drain);
            pending(&mut drain).await;
            for index in 0..CHANNEL_SIZE {
                listener.send_to(&index.to_le_bytes(), addr).await.unwrap();
            }
            let close = listener.close_session(addr);
            let blocked = listener.send_to(b"must not enqueue", addr);
            tokio::pin!(blocked);
            pending(&mut blocked).await;
            assert!(close.await.unwrap());
            assert!(blocked.await.is_err());
            assert!(drain.await.is_err());
        }
    })
    .await
    .expect("close waited for network confirmation");
}

#[tokio::test]
async fn captured_missing_target_and_old_idle_target_stay_missing() {
    let config =
        StreamConfig { session_idle_timeout: Duration::from_millis(60), ..StreamConfig::default() };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = peer.local_addr().unwrap();
    let missing = listener.close_session(addr);
    peer.send_to(&push_packet(41, 0, b"first"), listener.local_addr()).await.unwrap();
    assert_eq!(timeout(LIMIT, listener.recv()).await.unwrap().unwrap().0, b"first");
    let old = listener.close_session(addr);
    listener.send_to(b"unconfirmed", addr).await.unwrap();
    let pending_drain = listener.drain_to(addr, LIMIT);
    assert!(timeout(LIMIT, pending_drain).await.unwrap().is_err());
    peer.send_to(&push_packet(42, 0, b"second"), listener.local_addr()).await.unwrap();
    assert_eq!(timeout(LIMIT, listener.recv()).await.unwrap().unwrap().0, b"second");
    assert!(!old.await.unwrap());
    assert!(!missing.await.unwrap());
    listener.send_to(b"still active", addr).await.unwrap();
}

#[tokio::test]
async fn idle_cleanup_preserves_queued_receive_but_explicit_close_preserves_completed_result() {
    let config =
        StreamConfig { session_idle_timeout: Duration::from_millis(60), ..StreamConfig::default() };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
    let peer = peer(&mut listener, 41).await;
    let addr = peer.local_addr().unwrap();
    peer.send_to(&push_packet(41, 1, b"keep after idle"), listener.local_addr()).await.unwrap();
    receive_packet(&peer, KcpCmd::Ack).await;
    listener.send_to(b"unconfirmed", addr).await.unwrap();
    assert!(timeout(LIMIT, listener.drain_to(addr, LIMIT)).await.unwrap().is_err());
    assert_eq!(listener.recv().await.unwrap().0, b"keep after idle");

    peer.send_to(&push_packet(42, 0, b"already received"), listener.local_addr()).await.unwrap();
    let (ready, received) = tokio::sync::oneshot::channel();
    timeout(LIMIT, async {
        let data = listener.recv().await.unwrap();
        let close = listener.close_session(addr);
        ready.send(data).unwrap();
        assert!(close.await.unwrap());
    })
    .await
    .unwrap();
    assert_eq!(received.await.unwrap().0, b"already received");
}

#[tokio::test]
async fn listener_close_and_drop_wake_captured_requests_and_release_port() {
    for explicit in [false, true] {
        let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
        let listening = listener.local_addr();
        let peer = peer(&mut listener, 41).await;
        let request = listener.close_session(peer.local_addr().unwrap());
        if explicit {
            listener.close().unwrap();
        }
        drop(listener);
        assert!(timeout(LIMIT, request).await.unwrap().is_err());
        timeout(LIMIT, async {
            loop {
                if let Ok(socket) = UdpSocket::bind(listening).await {
                    drop(socket);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_clients_keep_order_and_payload_during_session_recycling() {
    timeout(Duration::from_secs(15), async {
        let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
        let expendable = peer(&mut listener, 99).await;
        let expendable_addr = expendable.local_addr().unwrap();
        let address = listener.local_addr();
        let server = tokio::spawn(async move {
            let mut received = [0usize; 20];
            let mut peers = [None; 20];
            let mut closed = false;
            for _ in 0..(20 * 64) {
                let (data, peer) = listener.recv().await.unwrap();
                let client = data[0] as usize;
                peers[client] = Some(peer);
                assert_eq!(data[1] as usize, received[client]);
                assert_eq!(data, payload(client, received[client]));
                received[client] += 1;
                listener.send_to(&data, peer).await.unwrap();
                if !closed {
                    assert!(listener.close_session(expendable_addr).await.unwrap());
                    closed = true;
                }
            }
            assert!(received.iter().all(|&messages| messages == 64));
            for peer in peers {
                listener.drain_to(peer.unwrap(), LIMIT).await.unwrap();
            }
            listener
        });
        let mut clients = tokio::task::JoinSet::new();
        for index in 0..20 {
            clients.spawn(async move {
                let mut client = KcpStream::connect(address).await.unwrap();
                for message in 0..64 {
                    let data = payload(index, message);
                    client.send(&data).await.unwrap();
                    assert_eq!(client.recv().await.unwrap(), data);
                }
                client.drain(LIMIT).await.unwrap();
                client
            });
        }
        let listener = server.await.unwrap();
        let mut retained = Vec::new();
        while let Some(result) = clients.join_next().await {
            retained.push(result.unwrap());
        }
        drop((listener, retained));
    })
    .await
    .unwrap();
}

fn payload(client: usize, message: usize) -> Vec<u8> {
    let mut data = vec![(client ^ message) as u8; 1024];
    data[0] = client as u8;
    data[1] = message as u8;
    data
}
