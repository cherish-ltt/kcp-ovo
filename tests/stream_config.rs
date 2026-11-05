//! 高层消息 API 的协议配置与实际 UDP 行为。

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use kcp_ovo::helper::{IKCP_OVERHEAD, KcpCmd, KcpPacketHeader};
use kcp_ovo::stream::{KcpListener, KcpStream, StreamConfig};
use kcp_ovo::{KcpConfig, KcpError};
use tokio::net::UdpSocket;
use tokio::time::timeout;

use common::stress::{config_matrix, control_packet, push_packet};
use common::udp::UdpRelay;
use common::{Net, Rng};

const LIMIT: Duration = Duration::from_secs(10);

fn transport() -> StreamConfig {
    StreamConfig {
        update_interval: Duration::from_millis(2),
        connect_timeout: Duration::from_secs(2),
        max_sessions: 32,
        session_idle_timeout: Duration::from_secs(30),
    }
}

#[tokio::test]
async fn invalid_protocol_configs_fail_before_address_resolution() {
    let invalid = [
        KcpConfig { mtu: 0, ..KcpConfig::fast_mode() },
        KcpConfig { mtu: 28, ..KcpConfig::fast_mode() },
        KcpConfig { mtu: 65_508, ..KcpConfig::fast_mode() },
        KcpConfig { mtu: u32::MAX, ..KcpConfig::fast_mode() },
        KcpConfig { snd_wnd: 0, ..KcpConfig::fast_mode() },
        KcpConfig { rcv_wnd: 0, ..KcpConfig::fast_mode() },
        KcpConfig::stream_mode(),
    ];
    for config in invalid {
        assert!(
            matches!(
                KcpStream::connect_with_kcp_config("invalid address", transport(), config.clone())
                    .await,
                Err(KcpError::InvalidConfig(_))
            ),
            "client accepted {config:?}"
        );
        assert!(
            matches!(
                KcpListener::bind_with_kcp_config("invalid address", transport(), config.clone())
                    .await,
                Err(KcpError::InvalidConfig(_))
            ),
            "listener accepted {config:?}"
        );
    }
    for mtu in [29, 65_507] {
        let config = KcpConfig { mtu, ..KcpConfig::fast_mode_with_wnd(1, 1) };
        let listener =
            KcpListener::bind_with_kcp_config("127.0.0.1:0", transport(), config.clone())
                .await
                .unwrap();
        let _client =
            KcpStream::connect_with_kcp_config(listener.local_addr(), transport(), config)
                .await
                .unwrap();
    }
}

#[tokio::test]
async fn protocol_matrix_preserves_messages_and_udp_mtu() {
    let mut tasks = tokio::task::JoinSet::new();
    for (index, case) in config_matrix().into_iter().filter(|case| !case.a.stream_mode).enumerate()
    {
        tasks.spawn(async move {
            let mut listener =
                KcpListener::bind_with_kcp_config("127.0.0.1:0", transport(), case.b.clone())
                    .await
                    .unwrap();
            let relay = UdpRelay::start(
                listener.local_addr(),
                Net::default().with_delay(1),
                index as u64 + 1,
            )
            .await;
            let mut client =
                KcpStream::connect_with_kcp_config(relay.addr, transport(), case.a.clone())
                    .await
                    .unwrap();
            let mut rng = Rng::new(index as u64 + 100);
            let request = rng.bytes((case.a.mtu as usize - IKCP_OVERHEAD) * 3);
            let response = rng.bytes((case.b.mtu as usize - IKCP_OVERHEAD) * 3);
            timeout(LIMIT, async {
                client.send(&request).await.unwrap();
                let (received, peer) = listener.recv().await.unwrap();
                assert_eq!(received, request, "{} request", case.name);
                listener.send_to(&response, peer).await.unwrap();
                assert_eq!(client.recv().await.unwrap(), response, "{} response", case.name);
            })
            .await
            .unwrap();
            let stats = *relay.stats.lock().unwrap();
            assert_eq!(stats.max_client_packet, case.a.mtu as usize, "{} client MTU", case.name);
            assert_eq!(stats.max_server_packet, case.b.mtu as usize, "{} server MTU", case.name);
            relay.stop().await;
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}

/// 不确认任何段，直到发生重传；整个观察期间的独立序号数就是实际在途窗口。
async fn observed_window(socket: &UdpSocket) -> (usize, u16, usize) {
    timeout(LIMIT, async {
        let mut buf = vec![0; 65_535];
        let mut sequences = HashSet::new();
        let mut max_size = 0;
        loop {
            let (size, source) = socket.recv_from(&mut buf).await.unwrap();
            let (header, _) = KcpPacketHeader::from_bytes(&mut &buf[..size]).unwrap();
            if header.cmd != KcpCmd::Push {
                continue;
            }
            max_size = max_size.max(size);
            if !sequences.insert(header.sn) {
                return (sequences.len(), header.wnd, max_size);
            }
            socket.send_to(&control_packet(header.conv, KcpCmd::Wins, 128), source).await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn legacy_defaults_and_custom_windows_are_visible_on_wire() {
    for mode in 0..4 {
        let config = match mode {
            2 => KcpConfig::normal_mode(),
            3 => KcpConfig { mtu: 128, ..KcpConfig::fast_mode_with_wnd(3, 7) },
            _ => KcpConfig::fast_mode(),
        };
        let expected = (
            if mode == 2 { 2 } else { config.snd_wnd as usize },
            config.rcv_wnd,
            config.mtu as usize,
        );
        let payload = Rng::new(40).bytes((config.mtu as usize - IKCP_OVERHEAD) * 40);
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let mut client = match mode {
            0 => KcpStream::connect(addr).await,
            1 => KcpStream::connect_with_config(addr, transport()).await,
            _ => KcpStream::connect_with_kcp_config(addr, transport(), config.clone()).await,
        }
        .unwrap();
        client.send(&payload).await.unwrap();
        assert_eq!(observed_window(&socket).await, expected, "client mode={mode}");
        client.close().unwrap();

        let mut listener = match mode {
            0 => KcpListener::bind("127.0.0.1:0").await,
            1 => KcpListener::bind_with_config("127.0.0.1:0", transport()).await,
            _ => KcpListener::bind_with_kcp_config("127.0.0.1:0", transport(), config).await,
        }
        .unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        peer.send_to(&push_packet(123, 0, b"start"), listener.local_addr()).await.unwrap();
        let (_, addr) = timeout(LIMIT, listener.recv()).await.unwrap().unwrap();
        listener.send_to(&payload, addr).await.unwrap();
        assert_eq!(observed_window(&peer).await, expected, "listener mode={mode}");
    }
}

#[tokio::test]
async fn maximum_message_and_rejected_send_preserve_order() {
    for mtu in [29, 128, 1400, 1500] {
        let config = KcpConfig { mtu, ..KcpConfig::fast_mode_with_wnd(255, 255) };
        let mut listener =
            KcpListener::bind_with_kcp_config("127.0.0.1:0", transport(), config.clone())
                .await
                .unwrap();
        let mut client =
            KcpStream::connect_with_kcp_config(listener.local_addr(), transport(), config)
                .await
                .unwrap();
        let max = (mtu as usize - IKCP_OVERHEAD) * 255;
        let payload = Rng::new(mtu as u64).bytes(max);
        let oversized = vec![0; max + 1];
        timeout(LIMIT, async {
            client.send(&payload).await.unwrap();
            assert!(matches!(client.send(&oversized).await, Err(KcpError::InvalidConfig(_))));
            assert!(matches!(client.try_send(&oversized), Err(KcpError::InvalidConfig(_))));
            client.try_send(b"x").unwrap();
            let (received, peer) = listener.recv().await.unwrap();
            assert_eq!(received, payload, "mtu={mtu}");
            assert_eq!(listener.recv().await.unwrap().0, b"x");
            listener.send_to(&payload, peer).await.unwrap();
            assert!(matches!(
                listener.send_to(&oversized, peer).await,
                Err(KcpError::InvalidConfig(_))
            ));
            listener.send_to(b"y", peer).await.unwrap();
            assert_eq!(client.recv().await.unwrap(), payload, "mtu={mtu}");
            assert_eq!(client.recv().await.unwrap(), b"y");
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn custom_protocol_recovers_from_loss_reordering_and_blackout() {
    for base in [KcpConfig::normal_mode(), KcpConfig::fast_mode()] {
        let config = KcpConfig { mtu: 128, snd_wnd: 16, rcv_wnd: 32, ..base };
        let mut listener =
            KcpListener::bind_with_kcp_config("127.0.0.1:0", transport(), config.clone())
                .await
                .unwrap();
        let net = Net::loss(0.05)
            .with_delay(2)
            .with_jitter(2)
            .with_reorder(0.2, 0.1)
            .with_blackout(0, 120);
        let relay = UdpRelay::start(listener.local_addr(), net, 0x40_AAAA).await;
        let mut client =
            KcpStream::connect_with_kcp_config(relay.addr, transport(), config).await.unwrap();
        let mut rng = Rng::new(0x40_BBBB);
        let started = tokio::time::Instant::now();
        let mut completed = 0;
        let result = timeout(Duration::from_secs(25), async {
            for _ in 0..10 {
                let payload = rng.bytes(1500);
                client.send(&payload).await.unwrap();
                let (received, peer) = listener.recv().await.unwrap();
                assert_eq!(received, payload);
                listener.send_to(&received, peer).await.unwrap();
                assert_eq!(client.recv().await.unwrap(), payload);
                completed += 1;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "completed {completed}/10 in {:?}; {:?}",
            started.elapsed(),
            relay.stats.lock().unwrap()
        );
        let stats = *relay.stats.lock().unwrap();
        assert!(stats.dropped > 0 && stats.blackout_dropped > 0);
        assert!(stats.reordered > 0 && stats.duplicated > 0);
        assert_eq!(stats.max_client_packet, 128);
        assert_eq!(stats.max_server_packet, 128);
        relay.stop().await;
    }
}

#[tokio::test]
async fn asymmetric_mtu_drains_both_directions_after_blackhole_timeout() {
    for client_fast in [false, true] {
        let normal = KcpConfig::normal_mode();
        let fast = KcpConfig::fast_mode();
        let (client_base, server_base) = if client_fast { (fast, normal) } else { (normal, fast) };
        let client_config = KcpConfig { mtu: 128, snd_wnd: 8, rcv_wnd: 16, ..client_base };
        let server_config = KcpConfig { mtu: 576, snd_wnd: 16, rcv_wnd: 32, ..server_base };
        let mut listener =
            KcpListener::bind_with_kcp_config("127.0.0.1:0", transport(), server_config)
                .await
                .unwrap();
        let blocked = Arc::new(AtomicBool::new(false));
        let discarded = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let filtering = blocked.clone();
        let observed = discarded.clone();
        let relay = UdpRelay::start_with_filter(
            listener.local_addr(),
            Net::default().with_delay(1),
            0x40_41,
            move |_, from_server| {
                let discard = filtering.load(Ordering::SeqCst);
                if discard {
                    observed[usize::from(from_server)].fetch_add(1, Ordering::SeqCst);
                }
                discard
            },
        )
        .await;
        let mut client = KcpStream::connect_with_kcp_config(relay.addr, transport(), client_config)
            .await
            .unwrap();
        timeout(LIMIT, async {
            client.send(b"ready").await.unwrap();
            let (ready, peer) = listener.recv().await.unwrap();
            assert_eq!(ready, b"ready");
            let (client_done, server_done) =
                tokio::join!(client.drain(LIMIT), listener.drain_to(peer, LIMIT));
            client_done.unwrap();
            server_done.unwrap();

            let request = Rng::new(40).bytes(2301);
            let response = Rng::new(41).bytes(5001);
            blocked.store(true, Ordering::SeqCst);
            client.send(&request).await.unwrap();
            listener.send_to(&response, peer).await.unwrap();
            // 超时跨过两个 normal 刷新周期，确保两个方向都实际经历黑洞。
            let wait = Duration::from_millis(200);
            let (client_done, server_done) =
                tokio::join!(client.drain(wait), listener.drain_to(peer, wait));
            assert_eq!(client_done.unwrap_err(), KcpError::TimedOut);
            assert_eq!(server_done.unwrap_err(), KcpError::TimedOut);
            for (direction, count) in discarded.iter().enumerate() {
                assert!(
                    count.load(Ordering::SeqCst) > 0,
                    "direction={direction}, client_fast={client_fast} must enter blackhole"
                );
            }

            blocked.store(false, Ordering::SeqCst);
            let (client_done, server_done, received, delivered) = tokio::join!(
                client.drain(LIMIT),
                listener.drain_to(peer, LIMIT),
                client.recv(),
                listener.recv(),
            );
            client_done.unwrap();
            server_done.unwrap();
            assert_eq!(received.unwrap(), response);
            assert_eq!(delivered.unwrap(), (request, peer));

            client.send(b"after timeout").await.unwrap();
            listener.send_to(b"after recovery", peer).await.unwrap();
            assert_eq!(listener.recv().await.unwrap(), (b"after timeout".to_vec(), peer));
            assert_eq!(client.recv().await.unwrap(), b"after recovery");
            let (client_done, server_done) =
                tokio::join!(client.drain(LIMIT), listener.drain_to(peer, LIMIT));
            client_done.unwrap();
            server_done.unwrap();
            client.close().unwrap();
            listener.close().unwrap();
        })
        .await
        .expect("asymmetric protocol drain recovery deadline");
        let stats = *relay.stats.lock().unwrap();
        assert_eq!(stats.max_client_packet, 128);
        assert_eq!(stats.max_server_packet, 576);
        relay.stop().await;
    }
}

#[tokio::test]
async fn twenty_custom_clients_transfer_one_mebibyte_in_chunks() {
    const CLIENTS: usize = 20;
    const CHUNKS: usize = 16;
    const CHUNK_SIZE: usize = 4096;
    const { assert!(CLIENTS * CHUNKS * CHUNK_SIZE >= 1024 * 1024) };
    let mut listener = KcpListener::bind_with_kcp_config(
        "127.0.0.1:0",
        transport(),
        KcpConfig { mtu: 576, ..KcpConfig::fast_mode_with_wnd(32, 64) },
    )
    .await
    .unwrap();
    let addr = listener.local_addr();
    let mut clients = tokio::task::JoinSet::new();
    for id in 0..CLIENTS {
        clients.spawn(async move {
            let config = KcpConfig { mtu: 256, ..KcpConfig::fast_mode_with_wnd(32, 64) };
            let mut client =
                KcpStream::connect_with_kcp_config(addr, transport(), config).await.unwrap();
            let mut rng = Rng::new(id as u64 + 1);
            for _ in 0..CHUNKS {
                let payload = rng.bytes(CHUNK_SIZE);
                client.send(&payload).await.unwrap();
                assert_eq!(client.recv().await.unwrap(), payload, "client {id}");
            }
        });
    }
    timeout(LIMIT, async {
        for _ in 0..CLIENTS * CHUNKS {
            let (data, addr) = listener.recv().await.unwrap();
            listener.send_to(&data, addr).await.unwrap();
        }
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
}
