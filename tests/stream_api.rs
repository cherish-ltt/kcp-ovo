//! 黑盒测试：Stream API 端到端
//!
//! 使用真实的 tokio + UDP socket 驱动 `KcpStream` / `KcpListener`，
//! 验证高层 API 的连接、收发、关闭与错误语义。

mod common;

use std::time::Duration;

use kcp_ovo::error::KcpErrorKind;
use kcp_ovo::stream::{KcpListener, KcpStream, StreamConfig};
use tokio::time::timeout;

#[tokio::test]
async fn zero_update_interval_is_rejected() {
    let config = StreamConfig { update_interval: Duration::ZERO, ..Default::default() };
    assert_eq!(
        KcpListener::bind_with_config("127.0.0.1:0", config.clone()).await.err().unwrap().kind(),
        KcpErrorKind::InvalidConfig
    );
    assert_eq!(
        KcpStream::connect_with_config("127.0.0.1:9", config).await.err().unwrap().kind(),
        KcpErrorKind::InvalidConfig
    );
    let config = StreamConfig { update_interval: Duration::from_nanos(1), ..Default::default() };
    let listener = KcpListener::bind_with_config("127.0.0.1:0", config.clone()).await.unwrap();
    let _stream = KcpStream::connect_with_config(listener.local_addr(), config).await.unwrap();
}

#[tokio::test]
async fn loopback_address_families_and_hostname() {
    for host in ["127.0.0.1", "::1"] {
        let addr = format!("[{host}]:0");
        let addr = if host == "127.0.0.1" { "127.0.0.1:0" } else { &addr };
        let mut listener = match KcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) if host == "::1" && tokio::net::UdpSocket::bind("[::1]:0").await.is_err() => {
                eprintln!("IPv6 loopback unavailable: {e}");
                continue;
            },
            Err(e) => panic!("listener bind failed: {e}"),
        };
        let mut stream = KcpStream::connect(listener.local_addr()).await.unwrap();
        stream.send(b"request").await.unwrap();
        let (data, peer) = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap();
        assert_eq!(data, b"request");
        listener.send_to(b"reply", peer).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(3), stream.recv()).await.unwrap().unwrap(),
            b"reply"
        );
    }
    let first = tokio::net::lookup_host("localhost:9").await.unwrap().next().unwrap();
    let bind = if first.is_ipv4() { "127.0.0.1:0" } else { "[::1]:0" };
    let mut listener = match KcpListener::bind(bind).await {
        Ok(listener) => listener,
        Err(e) if first.is_ipv6() && tokio::net::UdpSocket::bind("[::1]:0").await.is_err() => {
            eprintln!("IPv6 loopback unavailable for localhost: {e}");
            return;
        },
        Err(e) => panic!("hostname listener bind failed: {e}"),
    };
    let mut stream =
        KcpStream::connect(format!("localhost:{}", listener.local_addr().port())).await.unwrap();
    stream.send(b"hostname").await.unwrap();
    let (data, peer) = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap();
    assert_eq!(data, b"hostname");
    listener.send_to(b"reply", peer).await.unwrap();
    assert_eq!(timeout(Duration::from_secs(3), stream.recv()).await.unwrap().unwrap(), b"reply");
}

#[tokio::test]
async fn listener_rejects_invalid_session_limits() {
    for config in [
        StreamConfig { max_sessions: 0, ..Default::default() },
        StreamConfig { session_idle_timeout: Duration::ZERO, ..Default::default() },
    ] {
        assert!(KcpListener::bind_with_config("127.0.0.1:0", config).await.is_err());
    }
}

#[tokio::test]
async fn listener_limits_and_reclaims_sessions() {
    let config = StreamConfig {
        max_sessions: 1,
        session_idle_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
    let peer1 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer2 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr();
    let mut invalid_peers = Vec::new();
    for i in 0..40 {
        let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut data = common::stress::push_packet(1, 0, b"x");
        match i % 3 {
            0 => data.truncate(1),
            1 => data[4] = 255,
            _ => data[27] = 2,
        }
        peer.send_to(&data, addr).await.unwrap();
        invalid_peers.push(peer);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    peer1.send_to(b"x", addr).await.unwrap();
    peer2.send_to(&common::stress::push_packet(2, 0, b"first"), addr).await.unwrap();
    let (_, accepted) = timeout(Duration::from_secs(2), listener.recv()).await.unwrap().unwrap();
    assert_eq!(accepted, peer2.local_addr().unwrap());
    assert!(listener.send_to(b"x", peer1.local_addr().unwrap()).await.is_err());

    peer1.send_to(&common::stress::push_packet(1, 0, b"blocked"), addr).await.unwrap();
    assert!(timeout(Duration::from_millis(100), listener.recv()).await.is_err());
    tokio::time::sleep(Duration::from_millis(600)).await;
    peer1.send_to(&common::stress::push_packet(1, 0, b"after"), addr).await.unwrap();
    let (data, accepted) = timeout(Duration::from_secs(2), listener.recv()).await.unwrap().unwrap();
    assert_eq!(accepted, peer1.local_addr().unwrap());
    assert_eq!(data, b"after");
    assert!(listener.send_to(b"x", peer2.local_addr().unwrap()).await.is_err());
}

#[tokio::test]
async fn oversized_messages_are_rejected_before_enqueue() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut stream = KcpStream::connect(listener.local_addr()).await.unwrap();
    let max = 1372 * 255;
    let oversized = vec![42; max + 1];
    assert_eq!(stream.send(&oversized).await.unwrap_err().kind(), KcpErrorKind::InvalidConfig);
    assert_eq!(stream.try_send(&oversized).unwrap_err().kind(), KcpErrorKind::InvalidConfig);
    stream.send(b"marker").await.unwrap();
    let (message, peer) = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap();
    assert_eq!(message, b"marker");
    assert_eq!(
        listener.send_to(&oversized, peer).await.unwrap_err().kind(),
        KcpErrorKind::InvalidConfig
    );
    listener.send_to(b"reply", peer).await.unwrap();
    assert_eq!(timeout(Duration::from_secs(3), stream.recv()).await.unwrap().unwrap(), b"reply");
    assert!(stream.send(&[]).await.is_ok());
    assert!(stream.try_send(&vec![42; max]).is_ok());
    assert!(listener.send_to(&vec![42; max], peer).await.is_ok());
}

#[tokio::test]
async fn closed_stream_rejects_try_operations() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut stream = KcpStream::connect(listener.local_addr()).await.unwrap();
    stream.close().unwrap();
    stream.close().unwrap();
    assert!(stream.try_send(b"after close").is_err());
    assert!(stream.try_recv().is_err());
    assert!(timeout(Duration::from_millis(200), listener.recv()).await.is_err());
}

async fn rebind(addr: std::net::SocketAddr) {
    let socket = timeout(Duration::from_secs(3), async {
        loop {
            match tokio::net::UdpSocket::bind(addr).await {
                Ok(socket) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::task::yield_now().await
                },
                Err(e) => panic!("rebind failed: {e}"),
            }
        }
    })
    .await
    .unwrap();
    drop(socket);
}

#[tokio::test]
async fn listener_close_or_drop_releases_port_and_rejects_io() {
    for explicit_close in [true, false] {
        for count in [0, 20] {
            let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr();
            let mut clients = Vec::new();
            let mut peer = "127.0.0.1:1".parse().unwrap();
            for _ in 0..count {
                let mut client = KcpStream::connect(addr).await.unwrap();
                client.send(b"hello").await.unwrap();
                peer = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap().1;
                clients.push(client);
            }
            if explicit_close {
                listener.close().unwrap();
                listener.close().unwrap();
                assert!(listener.send_to(b"x", peer).await.is_err());
                assert!(timeout(Duration::from_secs(3), listener.recv()).await.unwrap().is_err());
                rebind(addr).await;
            }
            drop(listener);
            rebind(addr).await;
        }
    }
}

#[tokio::test]
async fn stream_close_or_drop_releases_socket() {
    for explicit_close in [true, false] {
        let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut stream = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
        let mut buf = [0u8; 1500];
        let (_, addr) =
            timeout(Duration::from_secs(3), peer.recv_from(&mut buf)).await.unwrap().unwrap();
        if explicit_close {
            stream.close().unwrap();
            rebind(addr).await;
        }
        drop(stream);
        rebind(addr).await;
    }
}

async fn wait_ack(socket: &tokio::net::UdpSocket, sn: u32) {
    let mut buf = [0u8; 1500];
    timeout(Duration::from_secs(3), async {
        loop {
            let (size, _) = socket.recv_from(&mut buf).await.unwrap();
            if size >= 28
                && buf[4] == u8::from(kcp_ovo::KcpCmd::Ack)
                && u32::from_be_bytes(buf[16..20].try_into().unwrap()) == sn
            {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn listener_delivers_all_messages_released_by_gap() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(&common::stress::push_packet(1, 1, b"b"), listener.local_addr()).await.unwrap();
    wait_ack(&peer, 1).await;
    peer.send_to(&common::stress::push_packet(1, 0, b"a"), listener.local_addr()).await.unwrap();
    for expected in [b"a", b"b"] {
        let (got, _) = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap();
        assert_eq!(&got, expected);
    }
}

#[tokio::test]
async fn client_delivers_all_messages_released_by_gap() {
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut stream = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
    let mut buf = [0u8; 1500];
    let (_, addr) =
        timeout(Duration::from_secs(3), peer.recv_from(&mut buf)).await.unwrap().unwrap();
    let conv = u32::from_be_bytes(buf[..4].try_into().unwrap());
    peer.send_to(&common::stress::push_packet(conv, 1, b"b"), addr).await.unwrap();
    wait_ack(&peer, 1).await;
    peer.send_to(&common::stress::push_packet(conv, 0, b"a"), addr).await.unwrap();
    for expected in [b"a", b"b"] {
        let got = timeout(Duration::from_secs(3), stream.recv()).await.unwrap().unwrap();
        assert_eq!(&got, expected);
    }
}

#[tokio::test]
async fn listener_recovers_after_invalid_fragment() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut invalid = common::stress::push_packet(1, 0, b"bad");
    invalid[5] = u8::MAX;
    peer.send_to(&invalid, listener.local_addr()).await.unwrap();
    wait_ack(&peer, 0).await;
    peer.send_to(&common::stress::push_packet(1, 1, b"ok"), listener.local_addr()).await.unwrap();
    let (got, _) = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap();
    assert_eq!(got, b"ok");
}

#[tokio::test]
async fn client_backpressure_keeps_protocol_progress_and_drains_messages() {
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut stream = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
    let mut buf = [0u8; 1500];
    let (_, addr) =
        timeout(Duration::from_secs(3), peer.recv_from(&mut buf)).await.unwrap().unwrap();
    let conv = u32::from_be_bytes(buf[..4].try_into().unwrap());
    for sn in 0..65 {
        peer.send_to(&common::stress::push_packet(conv, sn, &[sn as u8]), addr).await.unwrap();
        wait_ack(&peer, sn).await;
    }
    for _ in 0..65 {
        timeout(Duration::from_secs(3), stream.send(b"request")).await.unwrap().unwrap();
    }
    timeout(Duration::from_secs(3), async {
        loop {
            let (size, _) = peer.recv_from(&mut buf).await.unwrap();
            if size >= 28 && buf[4] == u8::from(kcp_ovo::KcpCmd::Push) {
                assert_eq!(&buf[28..size], b"request");
                break;
            }
        }
    })
    .await
    .unwrap();
    for expected in 0..65u8 {
        let got = timeout(Duration::from_secs(3), stream.recv()).await.unwrap().unwrap();
        assert_eq!(got, [expected]);
    }
}

#[tokio::test]
async fn listener_backpressure_keeps_protocol_progress_and_drains_messages() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for sn in 0..65 {
        peer.send_to(&common::stress::push_packet(1, sn, &[sn as u8]), listener.local_addr())
            .await
            .unwrap();
        wait_ack(&peer, sn).await;
    }
    listener.send_to(b"reply", peer.local_addr().unwrap()).await.unwrap();
    let mut buf = [0u8; 1500];
    timeout(Duration::from_secs(3), async {
        loop {
            let (size, _) = peer.recv_from(&mut buf).await.unwrap();
            if size >= 28 && buf[4] == u8::from(kcp_ovo::KcpCmd::Push) {
                assert_eq!(&buf[28..size], b"reply");
                break;
            }
        }
    })
    .await
    .unwrap();
    for expected in 0..65u8 {
        let (got, _) = timeout(Duration::from_secs(3), listener.recv()).await.unwrap().unwrap();
        assert_eq!(got, [expected]);
    }
}

/// 在随机端口上起一个回显服务，返回监听地址
async fn spawn_echo_server(rounds: usize) -> (String, tokio::sync::oneshot::Sender<()>) {
    let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().to_string();
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut listener = listener;
        for _ in 0..rounds {
            let Ok((data, peer)) = listener.recv().await else {
                break;
            };
            let _ = listener.send_to(&data, peer).await;
        }
        let _ = timeout(Duration::from_secs(10), stopped).await;
    });
    (addr, shutdown)
}

/// 客户端发送，服务端回显，客户端收到同样内容
#[tokio::test]
async fn echo_roundtrip() {
    let (addr, _shutdown) = spawn_echo_server(1).await;
    let mut stream = KcpStream::connect(&addr).await.unwrap();
    assert!(stream.is_connected());

    stream.send(b"echo-payload").await.unwrap();
    let got = timeout(Duration::from_secs(5), stream.recv()).await.unwrap().unwrap();
    assert_eq!(got, b"echo-payload");
}

/// 多轮请求-响应在同一连接上连续工作
#[tokio::test]
async fn echo_multiple_rounds() {
    let rounds = 5;
    let (addr, _shutdown) = spawn_echo_server(rounds).await;
    let mut stream = KcpStream::connect(&addr).await.unwrap();

    for i in 0..rounds {
        let msg = format!("round-{i}");
        stream.send(msg.as_bytes()).await.unwrap();
        let got = timeout(Duration::from_secs(5), stream.recv()).await.unwrap().unwrap();
        assert_eq!(got, msg.as_bytes(), "第 {i} 轮内容不一致");
    }
}

/// 大于单段的数据应完整回显（验证分片与重组）
#[tokio::test]
async fn echo_large_payload() {
    let (addr, _shutdown) = spawn_echo_server(1).await;
    let mut stream = KcpStream::connect(&addr).await.unwrap();

    let payload: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
    stream.send(&payload).await.unwrap();
    let got = timeout(Duration::from_secs(10), stream.recv()).await.unwrap().unwrap();
    assert_eq!(got, payload);
}

/// 并发多个客户端各自回显互不串扰
#[tokio::test]
async fn concurrent_clients_isolated() {
    let (addr, _shutdown) = spawn_echo_server(3).await;
    let mut handles = Vec::new();
    for i in 0..3u8 {
        let addr = addr.clone();
        handles.push(tokio::spawn(async move {
            let mut stream = KcpStream::connect(&addr).await.unwrap();
            let msg = vec![i; 400];
            stream.send(&msg).await.unwrap();
            let got = timeout(Duration::from_secs(8), stream.recv()).await.unwrap().unwrap();
            assert_eq!(got, msg, "客户端 {i} 数据被串扰");
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

/// close() 之后再收发必须返回明确的未连接错误
#[tokio::test]
async fn closed_stream_rejects_io() {
    let (addr, _shutdown) = spawn_echo_server(1).await;
    let mut stream = KcpStream::connect(&addr).await.unwrap();
    assert!(stream.is_connected());

    stream.close().unwrap();
    assert!(!stream.is_connected());

    assert!(stream.send(b"x").await.is_err());
    assert!(stream.recv().await.is_err());
}

/// remote_addr 应反映实际连接的对端地址
#[tokio::test]
async fn remote_addr_matches_target() {
    let (addr, _shutdown) = spawn_echo_server(1).await;
    let stream = KcpStream::connect(&addr).await.unwrap();
    assert_eq!(stream.remote_addr().to_string(), addr);
}

/// try_recv 在没有数据时应返回错误而不是阻塞
#[tokio::test]
async fn try_recv_without_data_errors() {
    let (addr, _shutdown) = spawn_echo_server(1).await;
    let mut stream = KcpStream::connect(&addr).await.unwrap();
    let err = stream.try_recv().unwrap_err();
    assert_eq!(err.kind(), KcpErrorKind::IoError);
}

/// 使用自定义 update_interval 的 StreamConfig 仍能正常工作
#[tokio::test]
async fn custom_stream_config_works() {
    let listener = KcpListener::bind_with_config(
        "127.0.0.1:0",
        StreamConfig {
            update_interval: Duration::from_millis(5),
            connect_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = listener.local_addr().to_string();
    let (_shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut listener = listener;
        if let Ok((data, peer)) = listener.recv().await {
            let _ = listener.send_to(&data, peer).await;
        }
        let _ = timeout(Duration::from_secs(10), stopped).await;
    });

    let mut stream = KcpStream::connect_with_config(
        &addr,
        StreamConfig {
            update_interval: Duration::from_millis(5),
            connect_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    stream.send(b"cfg").await.unwrap();
    let got = timeout(Duration::from_secs(5), stream.recv()).await.unwrap().unwrap();
    assert_eq!(got, b"cfg");
}

/// 非法地址必须返回错误而不是 panic
#[tokio::test]
async fn invalid_address_errors() {
    let err = KcpStream::connect("not a valid address:::123").await;
    assert!(err.is_err(), "非法地址应返回错误");
}

/// send_to 到未知地址必须返回错误，而不是静默丢弃
#[tokio::test]
async fn send_to_unknown_peer_errors() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let ghost: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let err = listener.send_to(b"x", ghost).await.unwrap_err();
    assert_eq!(err.kind(), KcpErrorKind::IoError);
}

/// local_addr 应回报实际绑定地址，且 0 端口已被系统分配
#[tokio::test]
async fn local_addr_reports_assigned_port() {
    let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr();
    assert_eq!(addr.ip().to_string(), "127.0.0.1");
    assert_ne!(addr.port(), 0, "系统必须分配真实端口");
}

/// listener.close() 后可继续调用而不 panic（幂等）
#[tokio::test]
async fn listener_close_is_idempotent() {
    let mut listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.close().unwrap();
    listener.close().unwrap();
}

/// 客户端在服务端未启动时 connect 不应 panic（UDP 无连接，connect 本身不失败）
#[tokio::test]
async fn connect_without_server_does_not_panic() {
    let result = KcpStream::connect("127.0.0.1:1").await;
    // UDP 的 connect 只设置默认对端，不做握手，因此可能成功
    if let Ok(mut s) = result {
        assert!(s.is_connected());
        let _ = s.close();
    }
}
