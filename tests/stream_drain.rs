//! 高层发送确认等待的黑盒契约。

mod common;

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use kcp_ovo::error::KcpErrorKind;
use kcp_ovo::helper::KcpPacketHeader;
use kcp_ovo::stream::{CHANNEL_SIZE, KcpListener, KcpStream, StreamConfig};
use kcp_ovo::{KcpCmd, KcpResult};
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout};

use common::stress::{RawHeader, push_packet, raw_packet};

const LIMIT: Duration = Duration::from_secs(3);

struct Peer {
    socket: UdpSocket,
    addr: SocketAddr,
    conv: u32,
}

impl Peer {
    async fn stream() -> (Self, KcpStream) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let stream = KcpStream::connect(socket.local_addr().unwrap()).await.unwrap();
        let mut buf = [0; 1500];
        let (len, addr) = timeout(LIMIT, socket.recv_from(&mut buf)).await.unwrap().unwrap();
        let (header, _) =
            KcpPacketHeader::from_bytes(&mut Bytes::copy_from_slice(&buf[..len])).unwrap();
        (Self { socket, addr, conv: header.conv }, stream)
    }

    async fn listener(listener: &mut KcpListener, conv: u32) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = Self { socket, addr: listener.local_addr(), conv };
        peer.socket.send_to(&push_packet(conv, 0, b"hello"), peer.addr).await.unwrap();
        let (data, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
        assert_eq!(data, b"hello");
        assert_eq!(addr, peer.socket.local_addr().unwrap());
        peer.receive(KcpCmd::Ack).await;
        peer
    }

    async fn receive(&self, cmd: KcpCmd) -> (KcpPacketHeader, Bytes) {
        timeout(LIMIT, async {
            let mut buf = [0; 1500];
            loop {
                let (len, addr) = self.socket.recv_from(&mut buf).await.unwrap();
                assert_eq!(addr, self.addr);
                let (header, payload) =
                    KcpPacketHeader::from_bytes(&mut Bytes::copy_from_slice(&buf[..len])).unwrap();
                if header.conv == self.conv && header.cmd == cmd {
                    return (header, payload);
                }
            }
        })
        .await
        .expect("peer packet deadline")
    }

    async fn acknowledge(&self, header: KcpPacketHeader, una: u32, wnd: u16) {
        let ack = RawHeader {
            conv: self.conv,
            cmd: KcpCmd::Ack as u8,
            sn: header.sn,
            ts: header.ts,
            una,
            wnd,
            ..RawHeader::default()
        };
        self.socket.send_to(&raw_packet(ack, &[]), self.addr).await.unwrap();
    }
}

async fn assert_pending(future: impl Future<Output = KcpResult<()>>) {
    tokio::select! {
        biased;
        result = future => panic!("drain completed before confirmation: {result:?}"),
        () = std::future::ready(()) => {},
    }
}

#[tokio::test]
async fn idle_stream_can_drain() {
    let (_peer, stream) = Peer::stream().await;
    timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = Peer::listener(&mut listener, 41).await;
    timeout(LIMIT, listener.drain_to(peer.socket.local_addr().unwrap(), LIMIT))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn queued_messages_and_lost_ack_cannot_complete_drain_early() {
    let (peer, mut stream) = Peer::stream().await;
    for byte in 0..4 {
        stream.try_send(&[byte]).unwrap();
    }
    let waiting = stream.drain(LIMIT);
    tokio::pin!(waiting);
    assert_pending(&mut waiting).await;
    let (first, data) = tokio::select! {
        result = &mut waiting => panic!("queued data was not confirmed: {result:?}"),
        packet = peer.receive(KcpCmd::Push) => packet,
    };
    assert_eq!(data.as_ref(), &[0]);
    let (retry, repeated) = tokio::select! {
        result = &mut waiting => panic!("lost ACK completed drain: {result:?}"),
        packet = peer.receive(KcpCmd::Push) => packet,
    };
    assert_eq!(retry.sn, first.sn);
    assert_eq!(repeated, data);
    peer.acknowledge(retry, 1, 128).await;
    let mut next = 1;
    timeout(LIMIT, async {
        while next < 4 {
            let (header, data) = tokio::select! {
                result = &mut waiting => panic!("partial confirmation completed drain: {result:?}"),
                packet = peer.receive(KcpCmd::Push) => packet,
            };
            if header.sn == next {
                assert_eq!(data.as_ref(), &[next as u8]);
                next += 1;
            }
            peer.acknowledge(header, next, 128).await;
        }
    })
    .await
    .expect("queued messages deadline");
    timeout(LIMIT, &mut waiting).await.unwrap().unwrap();
}

#[tokio::test]
async fn cancellation_keeps_data_and_later_sends_do_not_extend_the_barrier() {
    let (peer, mut stream) = Peer::stream().await;
    stream.send(b"before").await.unwrap();
    let (header, data) = {
        let cancelled = stream.drain(LIMIT);
        tokio::pin!(cancelled);
        assert_pending(&mut cancelled).await;
        tokio::select! {
            result = &mut cancelled => panic!("unconfirmed data completed drain: {result:?}"),
            packet = peer.receive(KcpCmd::Push) => packet,
        }
    };
    assert_eq!(data.as_ref(), b"before");
    let waiting = stream.drain(LIMIT);
    tokio::pin!(waiting);
    assert_pending(&mut waiting).await;
    stream.send(b"after").await.unwrap();
    peer.acknowledge(header, 1, 128).await;
    timeout(LIMIT, &mut waiting).await.unwrap().unwrap();
    let (header, data) = peer.receive(KcpCmd::Push).await;
    assert_eq!(data.as_ref(), b"after");
    let final_wait = stream.drain(LIMIT);
    tokio::pin!(final_wait);
    assert_pending(&mut final_wait).await;
    peer.acknowledge(header, 2, 128).await;
    timeout(LIMIT, final_wait).await.unwrap().unwrap();
}

#[tokio::test]
async fn deadline_includes_waiting_for_a_full_command_queue() {
    let (peer, mut stream) = Peer::stream().await;
    stream.send(b"unconfirmed").await.unwrap();
    let blocked = stream.drain(LIMIT);
    tokio::pin!(blocked);
    assert_pending(&mut blocked).await;
    peer.receive(KcpCmd::Push).await;
    for _ in 0..CHANNEL_SIZE {
        stream.try_send(b"queued").unwrap();
    }
    assert!(stream.try_send(b"queue full").is_err());
    let start = Instant::now();
    let result = timeout(Duration::from_secs(1), stream.drain(Duration::from_millis(50)))
        .await
        .expect("drain must include queue admission in its deadline");
    assert_eq!(result.unwrap_err().kind(), KcpErrorKind::TimedOut);
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert_pending(&mut blocked).await;
}

#[tokio::test]
async fn blackhole_times_out_without_discarding_data_and_zero_window_waits() {
    for zero_window in [false, true] {
        let (peer, mut stream) = Peer::stream().await;
        stream.send(b"first").await.unwrap();
        let (first, _) = peer.receive(KcpCmd::Push).await;
        if zero_window {
            peer.acknowledge(first, 1, 0).await;
            timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
            stream.send(b"after zero window").await.unwrap();
        }
        let error =
            timeout(LIMIT, stream.drain(Duration::from_millis(80))).await.unwrap().unwrap_err();
        assert_eq!(error.kind(), KcpErrorKind::TimedOut);
        peer.acknowledge(first, 1, 128).await;
        if zero_window {
            let (header, data) = peer.receive(KcpCmd::Push).await;
            assert_eq!(header.sn, 1);
            assert_eq!(data.as_ref(), b"after zero window");
            peer.acknowledge(header, 2, 128).await;
        }
        timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn close_and_drop_wake_client_and_listener_waiters() {
    for explicit_close in [false, true] {
        let (peer, mut stream) = Peer::stream().await;
        stream.send(b"unconfirmed").await.unwrap();
        let waiting = stream.drain(LIMIT);
        tokio::pin!(waiting);
        assert_pending(&mut waiting).await;
        peer.receive(KcpCmd::Push).await;
        if explicit_close {
            stream.close().unwrap();
        }
        drop(stream);
        let error = timeout(LIMIT, waiting).await.unwrap().unwrap_err();
        assert_eq!(error.kind(), KcpErrorKind::IoError);

        let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = Peer::listener(&mut listener, 41).await;
        let addr = peer.socket.local_addr().unwrap();
        listener.send_to(b"unconfirmed", addr).await.unwrap();
        let waiting = listener.drain_to(addr, LIMIT);
        tokio::pin!(waiting);
        assert_pending(&mut waiting).await;
        peer.receive(KcpCmd::Push).await;
        if explicit_close {
            listener.close().unwrap();
        }
        drop(listener);
        let error = timeout(LIMIT, waiting).await.unwrap().unwrap_err();
        assert_eq!(error.kind(), KcpErrorKind::IoError);
    }
}

#[tokio::test]
async fn reclaimed_and_recreated_address_cannot_complete_an_old_waiter() {
    let config = StreamConfig {
        session_idle_timeout: Duration::from_millis(100),
        ..StreamConfig::default()
    };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
    let mut peer = Peer::listener(&mut listener, 41).await;
    let addr = peer.socket.local_addr().unwrap();
    listener.send_to(b"old", addr).await.unwrap();
    let not_polled = listener.drain_to(addr, LIMIT);
    let waiting = listener.drain_to(addr, LIMIT);
    tokio::pin!(waiting);
    assert_pending(&mut waiting).await;
    peer.receive(KcpCmd::Push).await;
    assert_eq!(timeout(LIMIT, waiting).await.unwrap().unwrap_err().kind(), KcpErrorKind::IoError);
    peer.conv = 42;
    peer.socket.send_to(&push_packet(peer.conv, 0, b"new"), peer.addr).await.unwrap();
    let (data, new_addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
    assert_eq!(new_addr, addr);
    assert_eq!(data, b"new");
    assert_eq!(
        timeout(LIMIT, not_polled).await.unwrap().unwrap_err().kind(),
        KcpErrorKind::IoError
    );
    timeout(LIMIT, listener.drain_to(addr, LIMIT)).await.unwrap().unwrap();
    let (header, _) = peer.receive(KcpCmd::Ack).await;
    assert_eq!(header.una, 1);
}

#[tokio::test]
async fn listener_waits_for_confirmations_and_captures_missing_sessions_at_call_time() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let missing = listener.drain_to(addr, LIMIT);
    socket.send_to(&push_packet(41, 0, b"hello"), listener.local_addr()).await.unwrap();
    timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
    let peer = Peer { socket, addr: listener.local_addr(), conv: 41 };
    peer.receive(KcpCmd::Ack).await;
    assert_eq!(missing.await.unwrap_err().kind(), KcpErrorKind::IoError);
    listener.send_to(b"reply", addr).await.unwrap();
    let waiting = listener.drain_to(addr, LIMIT);
    tokio::pin!(waiting);
    assert_pending(&mut waiting).await;
    let (header, data) = tokio::select! {
        result = &mut waiting => panic!("listener reply was not confirmed: {result:?}"),
        packet = peer.receive(KcpCmd::Push) => packet,
    };
    assert_eq!(data.as_ref(), b"reply");
    let (retry, data) = tokio::select! {
        result = &mut waiting => panic!("listener completed after lost ACK: {result:?}"),
        packet = peer.receive(KcpCmd::Push) => packet,
    };
    assert_eq!(retry.sn, header.sn);
    assert_eq!(data.as_ref(), b"reply");
    peer.acknowledge(retry, 1, 128).await;
    timeout(LIMIT, waiting).await.unwrap().unwrap();
}

#[tokio::test]
async fn listener_timeout_and_cancellation_preserve_a_zero_window_queue() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = Peer::listener(&mut listener, 41).await;
    let addr = peer.socket.local_addr().unwrap();
    listener.send_to(b"first", addr).await.unwrap();
    let (first, data) = peer.receive(KcpCmd::Push).await;
    assert_eq!(data.as_ref(), b"first");
    peer.acknowledge(first, 1, 0).await;
    timeout(LIMIT, listener.drain_to(addr, LIMIT)).await.unwrap().unwrap();
    listener.send_to(b"waiting for window", addr).await.unwrap();
    {
        let cancelled = listener.drain_to(addr, LIMIT);
        tokio::pin!(cancelled);
        assert_pending(&mut cancelled).await;
    }
    let error = timeout(LIMIT, listener.drain_to(addr, Duration::from_millis(80)))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), KcpErrorKind::TimedOut);
    peer.acknowledge(first, 1, 128).await;
    let (header, data) = peer.receive(KcpCmd::Push).await;
    assert_eq!(header.sn, 1);
    assert_eq!(data.as_ref(), b"waiting for window");
    peer.acknowledge(header, 2, 128).await;
    timeout(LIMIT, listener.drain_to(addr, LIMIT)).await.unwrap().unwrap();
}

#[tokio::test]
async fn stalled_listener_drain_and_receive_backlog_do_not_block_twenty_clients() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = Peer::listener(&mut listener, 41).await;
    let stalled_addr = peer.socket.local_addr().unwrap();
    for sn in 1..=65 {
        peer.socket.send_to(&push_packet(peer.conv, sn, b"backlog"), peer.addr).await.unwrap();
        let (ack, _) = peer.receive(KcpCmd::Ack).await;
        assert_eq!(ack.sn, sn);
    }
    listener.send_to(b"stalled", stalled_addr).await.unwrap();
    let stalled = listener.drain_to(stalled_addr, Duration::from_secs(10));
    tokio::pin!(stalled);
    assert_pending(&mut stalled).await;
    peer.receive(KcpCmd::Push).await;

    let (confirmed, mut confirmations) = tokio::sync::mpsc::channel(20);
    let mut clients = tokio::task::JoinSet::new();
    for id in 0..20 {
        let confirmed = confirmed.clone();
        let server = listener.local_addr();
        clients.spawn(async move {
            let mut stream = KcpStream::connect(server).await.unwrap();
            let payload = [b'C', id];
            stream.send(&payload).await.unwrap();
            timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
            confirmed.send(()).await.unwrap();
            assert_eq!(timeout(LIMIT, stream.recv()).await.unwrap().unwrap(), payload);
            timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
        });
    }
    for _ in 0..20 {
        timeout(LIMIT, confirmations.recv()).await.unwrap().unwrap();
    }
    assert_pending(&mut stalled).await;
    let mut replies = 0;
    let mut backlog = 0;
    for _ in 0..85 {
        let (data, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
        if addr == stalled_addr {
            assert_eq!(data, b"backlog");
            backlog += 1;
        } else {
            assert_eq!(data.len(), 2);
            assert_eq!(data[0], b'C');
            listener.send_to(&data, addr).await.unwrap();
            timeout(LIMIT, listener.drain_to(addr, LIMIT)).await.unwrap().unwrap();
            replies += 1;
        }
    }
    assert_eq!(replies, 20);
    assert_eq!(backlog, 65);
    while let Some(result) = timeout(LIMIT, clients.join_next()).await.unwrap() {
        result.unwrap();
    }
    assert_pending(&mut stalled).await;
    listener.close().unwrap();
    assert_eq!(timeout(LIMIT, stalled).await.unwrap().unwrap_err().kind(), KcpErrorKind::IoError);
}

#[tokio::test]
async fn client_receive_backpressure_preserves_outgoing_drain_and_message_order() {
    let (peer, mut stream) = Peer::stream().await;
    for sn in 0..65 {
        peer.socket.send_to(&push_packet(peer.conv, sn, &[sn as u8]), peer.addr).await.unwrap();
        let (ack, _) = peer.receive(KcpCmd::Ack).await;
        assert_eq!(ack.sn, sn);
    }
    stream.send(b"outgoing").await.unwrap();
    let waiting = stream.drain(LIMIT);
    tokio::pin!(waiting);
    assert_pending(&mut waiting).await;
    let (header, data) = peer.receive(KcpCmd::Push).await;
    assert_eq!(data.as_ref(), b"outgoing");
    peer.acknowledge(header, 1, 128).await;
    timeout(LIMIT, waiting).await.unwrap().unwrap();
    for expected in 0..65 {
        assert_eq!(timeout(LIMIT, stream.recv()).await.unwrap().unwrap(), [expected]);
    }
    timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
}

#[tokio::test]
async fn drain_flushes_the_last_ack_before_immediate_close() {
    let (peer, mut stream) = Peer::stream().await;
    peer.socket.send_to(&push_packet(peer.conv, 0, b"last"), peer.addr).await.unwrap();
    assert_eq!(timeout(LIMIT, stream.recv()).await.unwrap().unwrap(), b"last");
    timeout(LIMIT, stream.drain(LIMIT)).await.unwrap().unwrap();
    stream.close().unwrap();
    let (ack, _) = peer.receive(KcpCmd::Ack).await;
    assert_eq!(ack.una, 1);

    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer = Peer { socket, addr: listener.local_addr(), conv: 41 };
    peer.socket.send_to(&push_packet(peer.conv, 0, b"last"), peer.addr).await.unwrap();
    let (data, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
    assert_eq!(data, b"last");
    timeout(LIMIT, listener.drain_to(addr, LIMIT)).await.unwrap().unwrap();
    listener.close().unwrap();
    let (ack, _) = peer.receive(KcpCmd::Ack).await;
    assert_eq!(ack.una, 1);
}

#[tokio::test]
async fn drain_flushes_cumulative_confirmation_after_receive_backpressure_before_close() {
    let (peer, mut stream) = Peer::stream().await;
    let count = CHANNEL_SIZE as u32 + kcp_ovo::KcpConfig::fast_mode().rcv_wnd as u32 + 1;
    let mut last_una = 0;
    for first in (0..count).step_by(16) {
        let end = (first + 16).min(count);
        for sn in first..end {
            peer.socket
                .send_to(&push_packet(peer.conv, sn, &sn.to_be_bytes()), peer.addr)
                .await
                .unwrap();
        }
        for _ in first..end {
            let (ack, _) = peer.receive(KcpCmd::Ack).await;
            last_una = last_una.max(ack.una);
        }
    }
    assert_eq!(last_una, count - 1, "last segment must remain behind the full receive queue");

    // 暂停下一次刷新，避免检查结果取决于墙钟更新时机。
    tokio::time::pause();
    timeout(LIMIT, async {
        for sn in 0..count {
            assert_eq!(stream.recv().await.unwrap(), sn.to_be_bytes());
        }
    })
    .await
    .expect("queued messages must remain readable");
    let waiting = stream.drain(LIMIT);
    tokio::pin!(waiting);
    for _ in 0..32 {
        assert_pending(&mut waiting).await;
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    timeout(LIMIT, waiting).await.unwrap().unwrap();
    stream.close().unwrap();
    timeout(LIMIT, async {
        loop {
            let (header, _) = peer.receive(KcpCmd::Wins).await;
            if header.una == count {
                break;
            }
        }
    })
    .await
    .expect("latest cumulative confirmation must be output before close");
}

#[tokio::test]
async fn one_megabyte_in_1024_messages_drains_after_loss_reordering_and_blackout() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let net = common::Net::loss(0.04)
        .with_delay(2)
        .with_jitter(3)
        .with_reorder(0.25, 0.1)
        .with_blackout(0, 80);
    let relay = common::udp::UdpRelay::start(listener.local_addr(), net, 0x4141).await;
    let mut stream = KcpStream::connect(relay.addr).await.unwrap();
    let limit = Duration::from_secs(15);
    timeout(limit, async {
        let server = async {
            let mut rng = common::Rng::new(0x41);
            let mut peer = None;
            for _ in 0..1024 {
                let (data, addr) = listener.recv().await.unwrap();
                assert_eq!(data, rng.bytes(1024));
                assert_eq!(*peer.get_or_insert(addr), addr);
            }
            let addr = peer.unwrap();
            listener.send_to(b"complete", addr).await.unwrap();
            listener.drain_to(addr, limit).await.unwrap();
        };
        let client = async {
            let mut rng = common::Rng::new(0x41);
            for _ in 0..1024 {
                stream.send(&rng.bytes(1024)).await.unwrap();
            }
            stream.drain(limit).await.unwrap();
            assert_eq!(stream.recv().await.unwrap(), b"complete");
            stream.drain(limit).await.unwrap();
        };
        tokio::join!(server, client);
    })
    .await
    .expect("weak-link transfer deadline");
    {
        let stats = relay.stats.lock().unwrap();
        assert!(stats.dropped > 0);
        assert!(stats.duplicated > 0);
        assert!(stats.reordered > 0);
        assert!(stats.blackout_dropped > 0);
    }
    relay.stop().await;
}
