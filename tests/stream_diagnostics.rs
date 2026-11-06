mod common;

use std::net::SocketAddr;
use std::time::Duration;

use kcp_ovo::helper::KcpPacketHeader;
use kcp_ovo::stream::{CHANNEL_SIZE, KcpDiagnostics, KcpListener, KcpStream, StreamConfig};
use kcp_ovo::{KcpCmd, KcpConfig, KcpResult};
use tokio::net::UdpSocket;
use tokio::time::timeout;

use common::stress::{RawHeader, control_packet, push_packet, raw_packet};
use common::udp::UdpRelay;
use common::{Net, Rng};

const LIMIT: Duration = Duration::from_secs(5);

async fn sample_when(
    mut query: impl FnMut() -> KcpResult<KcpDiagnostics>,
    predicate: impl Fn(&KcpDiagnostics) -> bool,
) -> KcpDiagnostics {
    timeout(LIMIT, async {
        loop {
            if let Ok(snapshot) = query()
                && predicate(&snapshot)
            {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("diagnostics condition was never sampled")
}

async fn receive_push(socket: &UdpSocket) -> (KcpPacketHeader, SocketAddr) {
    timeout(LIMIT, async {
        let mut buffer = [0; 1500];
        loop {
            let (size, source) = socket.recv_from(&mut buffer).await.unwrap();
            let (header, _) = KcpPacketHeader::from_bytes(&mut &buffer[..size]).unwrap();
            if header.cmd == KcpCmd::Push {
                return (header, source);
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn unseen_peer_is_unknown() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut client = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
    let snapshot = client.diagnostics().unwrap();
    assert_eq!(snapshot.smoothed_rtt_ms, None);
    assert_eq!(snapshot.remote_window_segments, None);
    assert_eq!(snapshot.queued_send_segments, 0);
    assert_eq!(snapshot.in_flight_segments, 0);
    assert!(snapshot.retransmission_timeout_ms > 0);
    assert_eq!(snapshot.congestion_window_segments, 2);
    assert!(snapshot.sampled_at.elapsed() < LIMIT);
    client.close().unwrap();
    assert!(client.diagnostics().is_err());
}

#[tokio::test]
async fn zero_window_and_backpressure_remain_queryable_behind_a_drain() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let protocol = KcpConfig { mtu: 128, ..KcpConfig::fast_mode_with_wnd(4, 7) };
    let mut client = KcpStream::connect_with_kcp_config(
        peer.local_addr().unwrap(),
        StreamConfig::default(),
        protocol,
    )
    .await
    .unwrap();
    client.send(&[7; 300]).await.unwrap();
    let (first, source) = receive_push(&peer).await;
    let before = sample_when(
        || client.diagnostics(),
        |snapshot| snapshot.queued_send_segments == 2 && snapshot.in_flight_segments == 1,
    )
    .await;
    assert_eq!(before.remote_window_segments, None);

    let waiting = client.drain(LIMIT);
    tokio::pin!(waiting);
    tokio::select! {
        biased;
        result = &mut waiting => panic!("unconfirmed drain completed: {result:?}"),
        () = std::future::ready(()) => {},
    }
    peer.send_to(&control_packet(first.conv, KcpCmd::Wins, 0), source).await.unwrap();
    sample_when(|| client.diagnostics(), |snapshot| snapshot.remote_window_segments == Some(0))
        .await;
    for _ in 0..CHANNEL_SIZE {
        client.try_send(b"after barrier").unwrap();
    }
    assert!(client.try_send(b"full").is_err());
    for _ in 0..1000 {
        let snapshot = client.diagnostics().unwrap();
        assert_eq!(snapshot.queued_send_segments + snapshot.in_flight_segments, 3);
        assert_eq!(snapshot.remote_window_segments, Some(0));
    }
    let ack = RawHeader {
        conv: first.conv,
        cmd: KcpCmd::Ack as u8,
        sn: first.sn,
        ts: first.ts,
        una: 1,
        wnd: 4,
        ..RawHeader::default()
    };
    peer.send_to(&raw_packet(ack, &[]), source).await.unwrap();
    let recovered = sample_when(
        || client.diagnostics(),
        |snapshot| {
            snapshot.remote_window_segments == Some(4)
                && snapshot.queued_send_segments + snapshot.in_flight_segments == 2
        },
    )
    .await;
    assert!(recovered.sampled_at >= before.sampled_at);
    client.close().unwrap();
    assert!(timeout(LIMIT, waiting).await.unwrap().is_err());
}

#[tokio::test]
async fn sessions_are_independent_and_recreation_has_fresh_state() {
    let transport = StreamConfig {
        session_idle_timeout: Duration::from_millis(150),
        ..StreamConfig::default()
    };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", transport).await.unwrap();
    let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let second = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let first_addr = first.local_addr().unwrap();
    let second_addr = second.local_addr().unwrap();
    assert!(listener.diagnostics_for(first_addr).is_err());
    for (socket, conv, window, payload) in
        [(&first, 51, 0, b"first".as_slice()), (&second, 52, 19, b"second".as_slice())]
    {
        let header = RawHeader { wnd: window, ..RawHeader::push(conv, 0) };
        socket.send_to(&raw_packet(header, payload), listener.local_addr()).await.unwrap();
        sample_when(
            || listener.diagnostics_for(socket.local_addr().unwrap()),
            |snapshot| snapshot.remote_window_segments == Some(window),
        )
        .await;
        let (received, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
        assert_eq!(received, payload);
        assert_eq!(addr, socket.local_addr().unwrap());
    }
    let old = listener.diagnostics_for(first_addr).unwrap();
    assert_eq!(old.smoothed_rtt_ms, None);
    assert_eq!(listener.diagnostics_for(second_addr).unwrap().remote_window_segments, Some(19));
    listener.send_to(b"queued in zero window", first_addr).await.unwrap();
    sample_when(
        || listener.diagnostics_for(first_addr),
        |snapshot| snapshot.queued_send_segments == 1 && snapshot.in_flight_segments == 0,
    )
    .await;
    let waiting = listener.drain_to(first_addr, LIMIT);
    assert!(timeout(LIMIT, waiting).await.unwrap().is_err());
    assert!(listener.diagnostics_for(first_addr).is_err());
    first.send_to(&push_packet(53, 0, b"replacement"), listener.local_addr()).await.unwrap();
    let (received, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
    assert_eq!(received, b"replacement");
    assert_eq!(addr, first_addr);
    let new = sample_when(
        || listener.diagnostics_for(first_addr),
        |snapshot| snapshot.remote_window_segments == Some(1024),
    )
    .await;
    assert!(new.sampled_at > old.sampled_at);
    assert_eq!(new.queued_send_segments, 0);
    assert_eq!(new.in_flight_segments, 0);
    assert_eq!(new.smoothed_rtt_ms, None);
    listener.close().unwrap();
    assert!(listener.diagnostics_for(first_addr).is_err());
}

#[tokio::test]
async fn queries_preserve_bidirectional_messages_on_damaged_and_blackout_links() {
    for network in [
        Net::default().with_delay(1),
        Net::loss(0.05).with_delay(2).with_reorder(0.1, 0.1),
        Net::default().with_blackout(0, 80),
    ] {
        let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = UdpRelay::start(listener.local_addr(), network, 4801).await;
        let mut client = KcpStream::connect(relay.addr).await.unwrap();
        let mut random = Rng::new(48);
        let payloads: Vec<_> = (0..64).map(|_| random.bytes(4096)).collect();
        timeout(LIMIT, async {
            for payload in &payloads {
                client.send(payload).await.unwrap();
                let _ = client.diagnostics();
            }
            let mut peer = None;
            for payload in &payloads {
                let (received, addr) = listener.recv().await.unwrap();
                assert_eq!(&received, payload);
                assert!(listener.diagnostics_for(addr).is_ok());
                listener.send_to(&received, addr).await.unwrap();
                peer = Some(addr);
            }
            for payload in &payloads {
                let _ = client.diagnostics();
                let response = client.recv().await.unwrap();
                assert_eq!(&response, payload);
            }
            client.drain(LIMIT).await.unwrap();
            listener.drain_to(peer.unwrap(), LIMIT).await.unwrap();
        })
        .await
        .unwrap();
        let snapshot = sample_when(
            || client.diagnostics(),
            |snapshot| snapshot.smoothed_rtt_ms.is_some() && snapshot.in_flight_segments == 0,
        )
        .await;
        assert_eq!(snapshot.queued_send_segments, 0);
        assert!(snapshot.remote_window_segments.is_some());
        relay.stop().await;
    }
}
