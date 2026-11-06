use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::thread::{self, ThreadId};
use std::time::Duration;

use bytes::Bytes;
use kcp_ovo::KcpError;
use kcp_ovo::stream::{
    KcpDiagnostics, KcpListener, KcpListenerSender, KcpSender, KcpStream, StreamConfig,
};
use tokio::time::timeout;

struct Addresses(Vec<SocketAddr>);

impl ToSocketAddrs for Addresses {
    type Iter = std::vec::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter> {
        Ok(self.0.clone().into_iter())
    }
}

#[derive(Clone, Copy)]
enum Resolution {
    Empty,
    Error,
    Panic,
}

struct CheckedResolver {
    runtime_thread: ThreadId,
    resolution: Resolution,
}

impl ToSocketAddrs for CheckedResolver {
    type Iter = std::vec::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter> {
        assert_ne!(thread::current().id(), self.runtime_thread);
        match self.resolution {
            Resolution::Empty => Ok(Vec::new().into_iter()),
            Resolution::Error => Err(io::Error::other("resolver error")),
            Resolution::Panic => panic!("resolver panic"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn address_resolution_runs_off_runtime_thread_and_preserves_failures() {
    for resolution in [Resolution::Empty, Resolution::Error, Resolution::Panic] {
        let resolver = CheckedResolver { runtime_thread: thread::current().id(), resolution };
        let error = match KcpStream::connect(resolver).await {
            Ok(_) => panic!("resolver failure unexpectedly connected"),
            Err(error) => error,
        };
        match resolution {
            Resolution::Empty => assert_eq!(error, KcpError::NoAddress),
            Resolution::Error => assert_eq!(error, KcpError::IoError("resolver error".into())),
            Resolution::Panic => {
                assert!(
                    matches!(error, KcpError::IoError(message) if message.contains("resolver panic"))
                );
            },
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_dependencies_and_standard_addresses_support_loopback() {
    timeout(Duration::from_secs(10), async {
        let occupied = UdpSocket::bind("127.0.0.1:0").unwrap();
        let occupied_addr = occupied.local_addr().unwrap();
        let addresses = Addresses(vec![occupied_addr, "127.0.0.1:0".parse().unwrap()]);
        let mut listener = tokio::spawn(KcpListener::bind(addresses)).await.unwrap().unwrap();
        assert_ne!(listener.local_addr(), occupied_addr);
        let address = listener.local_addr();
        let mut client = tokio::spawn(KcpStream::connect_with_config(
            Addresses(vec![address]),
            StreamConfig::default(),
        ))
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.remote_addr(), address);
        let initial: KcpDiagnostics = loop {
            if let Ok(snapshot) = client.diagnostics() {
                break snapshot;
            }
            tokio::task::yield_now().await;
        };
        let _: std::time::Instant = initial.sampled_at;
        assert_eq!(initial.smoothed_rtt_ms, None);

        let payload = Bytes::from(vec![0xA5; 4096]);
        client.send(payload.as_ref()).await.unwrap();
        let (received, peer): (Vec<u8>, SocketAddr) = listener.recv().await.unwrap();
        assert_eq!(received.as_slice(), payload.as_ref());
        let snapshot: KcpDiagnostics = loop {
            if let Ok(snapshot) = listener.diagnostics_for(peer) {
                break snapshot;
            }
            tokio::task::yield_now().await;
        };
        assert!(snapshot.remote_window_segments.is_some());
        listener.send_to(&received, peer).await.unwrap();
        let echoed: Vec<u8> = client.recv().await.unwrap();
        assert_eq!(echoed.as_slice(), payload.as_ref());
        let owned_by_downstream = Bytes::from(echoed);
        assert_eq!(owned_by_downstream, payload);

        client.try_send(b"next message").unwrap();
        let (received, next_peer): (Vec<u8>, SocketAddr) = listener.recv().await.unwrap();
        assert_eq!(next_peer, peer);
        assert_eq!(received, b"next message");
        listener.send_to(&received, peer).await.unwrap();
        let received: Vec<u8> = loop {
            match client.try_recv() {
                Ok(received) => break received,
                Err(error) if client.is_connected() => {
                    assert!(error.to_string().contains("empty"), "{error}");
                    tokio::task::yield_now().await;
                },
                Err(error) => panic!("receive failed: {error}"),
            }
        };
        assert_eq!(received, b"next message");

        let sender: KcpSender = client.sender();
        let listener_sender: KcpListenerSender = listener.sender();
        let request = tokio::spawn(sender.send(b"independent"));
        let (received, peer) = listener.recv().await.unwrap();
        request.await.unwrap().unwrap();
        let response = tokio::spawn(listener_sender.send_to(&received, peer));
        assert_eq!(client.recv().await.unwrap(), b"independent");
        response.await.unwrap().unwrap();
        sender.drain(Duration::from_secs(2)).await.unwrap();
        listener_sender.drain_to(peer, Duration::from_secs(2)).await.unwrap();
        client.close().unwrap();
        assert!(sender.try_send(b"closed").is_err());
        listener.close().unwrap();
        assert!(listener_sender.send_to(b"closed", peer).await.is_err());
    })
    .await
    .expect("loopback transfer timed out");
}
