//! 黑盒测试：Stream API 端到端
//!
//! 使用真实的 tokio + UDP socket 驱动 `KcpStream` / `KcpListener`，
//! 验证高层 API 的连接、收发、关闭与错误语义。

mod common;

use std::time::Duration;

use kcp_ovo::error::KcpErrorKind;
use kcp_ovo::stream::{KcpListener, KcpStream, StreamConfig};
use tokio::time::timeout;

/// 在随机端口上起一个回显服务，返回监听地址
async fn spawn_echo_server(rounds: usize) -> String {
    let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().to_string();
    tokio::spawn(async move {
        let mut listener = listener;
        for _ in 0..rounds {
            let Ok((data, peer)) = listener.recv().await else {
                break;
            };
            let _ = listener.send_to(&data, peer).await;
        }
    });
    addr
}

/// 客户端发送，服务端回显，客户端收到同样内容
#[tokio::test]
async fn echo_roundtrip() {
    let addr = spawn_echo_server(1).await;
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
    let addr = spawn_echo_server(rounds).await;
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
    let addr = spawn_echo_server(1).await;
    let mut stream = KcpStream::connect(&addr).await.unwrap();

    let payload: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
    stream.send(&payload).await.unwrap();
    let got = timeout(Duration::from_secs(10), stream.recv()).await.unwrap().unwrap();
    assert_eq!(got, payload);
}

/// 并发多个客户端各自回显互不串扰
#[tokio::test]
async fn concurrent_clients_isolated() {
    let addr = spawn_echo_server(3).await;
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
    let addr = spawn_echo_server(1).await;
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
    let addr = spawn_echo_server(1).await;
    let stream = KcpStream::connect(&addr).await.unwrap();
    assert_eq!(stream.remote_addr().to_string(), addr);
}

/// try_recv 在没有数据时应返回错误而不是阻塞
#[tokio::test]
async fn try_recv_without_data_errors() {
    let addr = spawn_echo_server(1).await;
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
        },
    )
    .await
    .unwrap();
    let addr = listener.local_addr().to_string();
    tokio::spawn(async move {
        let mut listener = listener;
        if let Ok((data, peer)) = listener.recv().await {
            let _ = listener.send_to(&data, peer).await;
        }
    });

    let mut stream = KcpStream::connect_with_config(
        &addr,
        StreamConfig {
            update_interval: Duration::from_millis(5),
            connect_timeout: Duration::from_secs(3),
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
