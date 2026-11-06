#![cfg(feature = "stream")]

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::task::Poll;
use std::time::Duration;

use kcp_ovo::helper::{IKCP_OVERHEAD, KcpPacketHeader};
use kcp_ovo::stream::{CHANNEL_SIZE, KcpListener, KcpStream, StreamConfig};
use kcp_ovo::{KcpCmd, KcpConfig, KcpError, KcpResult};
use tokio::net::UdpSocket;
use tokio::time::timeout;

use common::stress::{RawHeader, push_packet, raw_packet};
use common::udp::UdpRelay;
use common::{Net, Rng};

const LIMIT: Duration = Duration::from_secs(10);

async fn pending(future: impl Future<Output = KcpResult<()>>) {
    tokio::pin!(future);
    assert!(
        std::future::poll_fn(|context| Poll::Ready(future.as_mut().poll(context)))
            .await
            .is_pending()
    );
}

async fn receive(socket: &UdpSocket, cmd: KcpCmd) -> (KcpPacketHeader, Vec<u8>, SocketAddr) {
    timeout(LIMIT, async {
        let mut buffer = vec![0; 65_535];
        loop {
            let (size, addr) = socket.recv_from(&mut buffer).await.unwrap();
            let (header, payload) = KcpPacketHeader::from_bytes(&mut &buffer[..size]).unwrap();
            if header.cmd == cmd {
                return (header, payload.to_vec(), addr);
            }
        }
    })
    .await
    .unwrap()
}

async fn acknowledge(socket: &UdpSocket, addr: SocketAddr, header: KcpPacketHeader, una: u32) {
    let ack = RawHeader {
        conv: header.conv,
        cmd: KcpCmd::Ack as u8,
        wnd: 128,
        sn: header.sn,
        ts: header.ts,
        una,
        ..RawHeader::default()
    };
    socket.send_to(&raw_packet(ack, &[]), addr).await.unwrap();
}

#[tokio::test]
async fn sender_handles_work_in_separate_tasks_and_dropping_them_keeps_owner_open() {
    timeout(LIMIT, async {
        let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut stream = KcpStream::connect(listener.local_addr()).await.unwrap();
        let sender = stream.sender();
        let outgoing = tokio::spawn(async move { sender.send(b"hello").await.unwrap() });
        let (message, peer) = listener.recv().await.unwrap();
        assert_eq!(message, b"hello");
        outgoing.await.unwrap();
        let sender = listener.sender();
        let reply = tokio::spawn(async move { sender.send_to(b"reply", peer).await.unwrap() });
        assert_eq!(stream.recv().await.unwrap(), b"reply");
        reply.await.unwrap();
        stream.send(b"owner still open").await.unwrap();
        assert_eq!(listener.recv().await.unwrap().0, b"owner still open");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_clients_and_multiple_producers_preserve_order_and_session_isolation() {
    const CLIENTS: usize = 20;
    const PRODUCERS: usize = 2;
    const MESSAGES: usize = 64;
    const LENGTH: usize = 512;
    timeout(LIMIT, async {
        let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr();
        let replies = listener.sender();
        let server = tokio::spawn(async move {
            let mut next = HashMap::new();
            let mut peers = HashMap::new();
            let mut received_bytes = 0;
            for _ in 0..CLIENTS * PRODUCERS * MESSAGES {
                let (message, peer) = listener.recv().await.unwrap();
                assert_eq!(message.len(), LENGTH);
                received_bytes += message.len();
                let client = message[0] as usize;
                let producer = message[1] as usize;
                let sequence = message[2] as usize;
                assert!(client < CLIENTS && producer < PRODUCERS);
                assert_eq!(*peers.entry(client).or_insert(peer), peer);
                let expected = next.entry((client, producer)).or_insert(0);
                assert_eq!(sequence, *expected);
                *expected += 1;
                let reply = replies.clone();
                tokio::spawn(async move { reply.send_to(&message, peer).await.unwrap() })
                    .await
                    .unwrap();
            }
            for peer in peers.values() {
                replies.drain_to(*peer, LIMIT).await.unwrap();
            }
            assert!(received_bytes >= 1024 * 1024);
            listener
        });
        let mut clients = tokio::task::JoinSet::new();
        for client_id in 0..CLIENTS {
            clients.spawn(async move {
                let mut client = KcpStream::connect(address).await.unwrap();
                let mut producers = tokio::task::JoinSet::new();
                for producer in 0..PRODUCERS {
                    let sender = client.sender();
                    producers.spawn(async move {
                        let mut random = Rng::new((client_id * PRODUCERS + producer + 1) as u64);
                        for sequence in 0..MESSAGES {
                            let mut payload = random.bytes(LENGTH);
                            payload[..3].copy_from_slice(&[
                                client_id as u8,
                                producer as u8,
                                sequence as u8,
                            ]);
                            sender.send(&payload).await.unwrap();
                        }
                        sender.drain(LIMIT).await.unwrap();
                    });
                }
                let mut next = [0; PRODUCERS];
                let mut random = [
                    Rng::new((client_id * PRODUCERS + 1) as u64),
                    Rng::new((client_id * PRODUCERS + 2) as u64),
                ];
                for _ in 0..PRODUCERS * MESSAGES {
                    let received = client.recv().await.unwrap();
                    let producer = received[1] as usize;
                    assert!(producer < PRODUCERS);
                    let mut expected = random[producer].bytes(LENGTH);
                    expected[..3].copy_from_slice(&[
                        client_id as u8,
                        producer as u8,
                        next[producer],
                    ]);
                    assert_eq!(received, expected);
                    next[producer] += 1;
                }
                while let Some(result) = producers.join_next().await {
                    result.unwrap();
                }
                client.drain(LIMIT).await.unwrap();
                client
            });
        }
        let listener = server.await.unwrap();
        let mut owners = Vec::new();
        while let Some(result) = clients.join_next().await {
            owners.push(result.unwrap());
        }
        drop((owners, listener));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn full_fifo_keeps_receiving_and_excludes_cancelled_and_post_barrier_sends() {
    timeout(LIMIT, async {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut client = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
        let sender = client.sender();
        sender.send(b"before").await.unwrap();
        let barrier = sender.drain(LIMIT);
        tokio::pin!(barrier);
        pending(&mut barrier).await;
        let (first, message, address) = receive(&peer, KcpCmd::Push).await;
        assert_eq!(message, b"before");
        for sequence in 0..CHANNEL_SIZE {
            sender.try_send(&[sequence as u8]).unwrap();
        }
        assert!(sender.try_send(b"full").is_err());
        let mut cancelled = Box::pin(sender.send(b"cancelled"));
        pending(&mut cancelled).await;
        drop(cancelled);
        let queued = sender.send(b"last");
        tokio::pin!(queued);
        pending(&mut queued).await;
        peer.send_to(&push_packet(first.conv, 0, b"receive while full"), address).await.unwrap();
        assert_eq!(client.recv().await.unwrap(), b"receive while full");
        acknowledge(&peer, address, first, 1).await;
        barrier.await.unwrap();
        queued.await.unwrap();
        let mut next = 1;
        while next <= CHANNEL_SIZE + 1 {
            let (header, payload, _) = receive(&peer, KcpCmd::Push).await;
            if header.sn < next as u32 {
                continue;
            }
            assert_eq!(header.sn, next as u32);
            if next <= CHANNEL_SIZE {
                assert_eq!(payload, [next as u8 - 1]);
            } else {
                assert_eq!(payload, b"last");
            }
            next += 1;
            acknowledge(&peer, address, header, next as u32).await;
        }
        sender.drain(LIMIT).await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn oversized_messages_fail_before_queue_and_blackout_recovers() {
    timeout(LIMIT, async {
        let protocol = KcpConfig { mtu: 128, ..KcpConfig::fast_mode_with_wnd(32, 64) };
        let mut listener = KcpListener::bind_with_kcp_config(
            "127.0.0.1:0",
            StreamConfig::default(),
            protocol.clone(),
        )
        .await
        .unwrap();
        let relay =
            UdpRelay::start(listener.local_addr(), Net::default().with_blackout(0, 200), 149).await;
        let mut client =
            KcpStream::connect_with_kcp_config(relay.addr, StreamConfig::default(), protocol)
                .await
                .unwrap();
        let sender = client.sender();
        let oversized = vec![0; (128 - IKCP_OVERHEAD) * 255 + 1];
        assert!(matches!(sender.send(&oversized).await, Err(KcpError::InvalidConfig(_))));
        assert!(matches!(sender.try_send(&oversized), Err(KcpError::InvalidConfig(_))));
        sender.send(b"first").await.unwrap();
        assert_eq!(sender.drain(Duration::from_millis(30)).await, Err(KcpError::TimedOut));
        let mut cancelled = Box::pin(sender.drain(LIMIT));
        pending(&mut cancelled).await;
        drop(cancelled);
        sender.send(b"second").await.unwrap();
        let replies = listener.sender();
        for expected in [b"first".as_slice(), b"second".as_slice()] {
            let (message, peer) = listener.recv().await.unwrap();
            assert_eq!(message, expected);
            assert!(matches!(
                replies.send_to(&oversized, peer).await,
                Err(KcpError::InvalidConfig(_))
            ));
            replies.send_to(&message, peer).await.unwrap();
            assert_eq!(client.recv().await.unwrap(), message);
        }
        sender.drain(LIMIT).await.unwrap();
        assert!(relay.stats.lock().unwrap().blackout_dropped > 0);
        relay.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owner_close_and_drop_wake_waiters_and_release_sockets_with_handles_alive() {
    timeout(LIMIT, async {
        for explicit in [false, true] {
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let mut client = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
            let sender = client.sender();
            let retained = sender.clone();
            sender.send(b"unconfirmed").await.unwrap();
            let barrier = sender.drain(LIMIT);
            tokio::pin!(barrier);
            pending(&mut barrier).await;
            let (_, _, address) = receive(&peer, KcpCmd::Push).await;
            for _ in 0..CHANNEL_SIZE {
                sender.try_send(b"queued").unwrap();
            }
            let waiting = sender.send(b"blocked");
            tokio::pin!(waiting);
            pending(&mut waiting).await;
            if explicit {
                client.close().unwrap();
            }
            drop(client);
            assert!(waiting.await.is_err());
            assert!(barrier.await.is_err());
            assert!(retained.send(b"closed").await.is_err());
            assert!(retained.try_send(b"closed").is_err());
            loop {
                if let Ok(socket) = UdpSocket::bind(address).await {
                    drop(socket);
                    break;
                }
                tokio::task::yield_now().await;
            }
            let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr();
            peer.send_to(&push_packet(49, 0, b"hello"), address).await.unwrap();
            let (_, target) = listener.recv().await.unwrap();
            let sender = listener.sender();
            sender.send_to(b"unconfirmed", target).await.unwrap();
            let barrier = sender.drain_to(target, LIMIT);
            tokio::pin!(barrier);
            pending(&mut barrier).await;
            receive(&peer, KcpCmd::Push).await;
            for _ in 0..CHANNEL_SIZE {
                sender.send_to(b"queued", target).await.unwrap();
            }
            let waiting = sender.send_to(b"blocked", target);
            tokio::pin!(waiting);
            pending(&mut waiting).await;
            if explicit {
                listener.close().unwrap();
            }
            drop(listener);
            assert!(waiting.await.is_err());
            assert!(barrier.await.is_err());
            assert!(sender.send_to(b"closed", target).await.is_err());
            loop {
                if let Ok(socket) = UdpSocket::bind(address).await {
                    drop(socket);
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn old_listener_operations_cannot_migrate_to_recreated_address() {
    timeout(LIMIT, async {
        let config = StreamConfig {
            session_idle_timeout: Duration::from_millis(100),
            ..StreamConfig::default()
        };
        let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr();
        let target = peer.local_addr().unwrap();
        let sender = listener.sender();
        let missing = sender.send_to(b"not a creator", target);
        peer.send_to(&push_packet(49, 0, b"old"), address).await.unwrap();
        assert_eq!(listener.recv().await.unwrap().0, b"old");
        assert!(missing.await.is_err());
        let captured = sender.send_to(b"must stay old", target);
        let captured_drain = sender.drain_to(target, LIMIT);
        sender.send_to(b"unconfirmed", target).await.unwrap();
        let barrier = sender.drain_to(target, LIMIT);
        tokio::pin!(barrier);
        pending(&mut barrier).await;
        receive(&peer, KcpCmd::Push).await;
        for _ in 0..CHANNEL_SIZE {
            sender.send_to(b"queued", target).await.unwrap();
        }
        let waiting = sender.send_to(b"blocked", target);
        tokio::pin!(waiting);
        pending(&mut waiting).await;
        assert!(barrier.await.is_err());
        assert!(waiting.await.is_err());
        peer.send_to(&push_packet(50, 0, b"new"), address).await.unwrap();
        assert_eq!(listener.recv().await.unwrap().0, b"new");
        assert!(captured.await.is_err());
        assert!(captured_drain.await.is_err());
        sender.send_to(b"new only", target).await.unwrap();
        let (header, payload, _) = receive(&peer, KcpCmd::Push).await;
        assert_eq!(header.conv, 50);
        assert_eq!(header.sn, 0);
        assert_eq!(payload, b"new only");
        acknowledge(&peer, address, header, 1).await;
        sender.drain_to(target, LIMIT).await.unwrap();
    })
    .await
    .unwrap();
}
