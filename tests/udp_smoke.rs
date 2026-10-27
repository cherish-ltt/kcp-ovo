//! 黑盒测试：真实 UDP 端到端冒烟
//!
//! 前面的用例都用虚拟时钟，快且确定；这里补上唯一无法用虚拟时钟覆盖的部分：
//! 真实的 tokio 任务调度、真实的 socket 收发、真实的时钟推进。
//! 用例保持小而快，任何整体超过数秒的等待都视为失败信号。

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::{Rng, assert_same};
use kcp_ovo::stream::{KcpListener, KcpStream, StreamConfig};
use kcp_ovo::{KcpConfig, KcpError};
use tokio::time::timeout;

const SHORT: Duration = Duration::from_secs(5);

/// 绑定本地随机端口，返回监听器与实际地址
async fn bind_listener() -> (KcpListener, SocketAddr) {
    let config = StreamConfig {
        update_interval: Duration::from_millis(5),
        connect_timeout: Duration::from_secs(2),
    };
    let listener = KcpListener::bind_with_config("127.0.0.1:0", config).await.unwrap();
    let addr = listener.local_addr();
    (listener, addr)
}

async fn connect(addr: SocketAddr) -> KcpStream {
    let config = StreamConfig {
        update_interval: Duration::from_millis(5),
        connect_timeout: Duration::from_secs(2),
    };
    KcpStream::connect_with_config(addr, config).await.unwrap()
}

/// 最简单的端到端：发一条，收一条，两端地址视角互相吻合
#[tokio::test]
async fn udp_smoke_single_message() {
    let (mut listener, addr) = bind_listener().await;
    let mut client = connect(addr).await;

    let payload = b"hello over real udp".to_vec();
    timeout(SHORT, client.send(&payload)).await.unwrap().unwrap();

    let (got, peer) = timeout(SHORT, listener.recv()).await.unwrap().unwrap();
    assert_same(&got, &payload);
    // 客户端的 remote_addr 是服务端地址，服务端看到的 peer 是客户端源地址
    assert_eq!(client.remote_addr(), addr);
    assert_ne!(peer, addr, "服务端应记录客户端源地址而不是自身地址");
}

/// 多轮往返：验证长时间任务调度下双方 KCP 状态都还能推进
#[tokio::test]
async fn udp_smoke_multiple_rounds() {
    let (mut listener, addr) = bind_listener().await;
    let mut client = connect(addr).await;

    for i in 0..10u8 {
        let payload = vec![i; 16];
        timeout(SHORT, client.send(&payload)).await.unwrap().unwrap();
        let (got, peer) = timeout(SHORT, listener.recv()).await.unwrap().unwrap();
        assert_same(&got, &payload);
        // 回一发，验证反方向通道同样畅通
        timeout(SHORT, listener.send_to(&[i + 100; 8], peer)).await.unwrap().unwrap();
        let reply = timeout(SHORT, client.recv()).await.unwrap().unwrap();
        assert_same(&reply, &[i + 100; 8]);
    }
}

/// 大载荷：跨越多次 update 与多个分片
#[tokio::test]
async fn udp_smoke_large_payload() {
    let (mut listener, addr) = bind_listener().await;
    let mut client = connect(addr).await;

    let payload = Rng::new(0x5551).bytes(20_000);
    timeout(SHORT, client.send(&payload)).await.unwrap().unwrap();

    let mut got = Vec::new();
    let deadline = tokio::time::Instant::now() + SHORT;
    while got.len() < payload.len() {
        let (chunk, _) = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            listener.recv(),
        )
        .await
        .expect("大载荷接收超时")
        .unwrap();
        got.extend_from_slice(&chunk);
    }
    assert_same(&got, &payload);
}

/// 多个客户端并发：互不串数据，各自地址独立
#[tokio::test]
async fn udp_smoke_concurrent_clients() {
    let (mut listener, addr) = bind_listener().await;
    let mut clients = Vec::new();
    for _ in 0..3 {
        clients.push(connect(addr).await);
    }

    for (i, c) in clients.iter_mut().enumerate() {
        let payload = format!("client-{i}").into_bytes();
        timeout(SHORT, c.send(&payload)).await.unwrap().unwrap();
    }

    let mut seen = std::collections::HashMap::new();
    for _ in 0..clients.len() {
        let (got, peer) = timeout(SHORT, listener.recv()).await.unwrap().unwrap();
        seen.insert(peer, String::from_utf8(got).unwrap());
    }
    assert_eq!(seen.len(), clients.len(), "不同客户端应来自不同地址");
    let mut values: Vec<_> = seen.values().cloned().collect();
    values.sort();
    assert_eq!(values, vec!["client-0", "client-1", "client-2"]);
}

/// 关闭后的 stream 必须拒绝收发，而不是静默丢弃
#[tokio::test]
async fn udp_smoke_closed_stream_rejects_io() {
    let (_listener, addr) = bind_listener().await;
    let mut client = connect(addr).await;
    assert!(client.is_connected());
    client.close().unwrap();
    assert!(!client.is_connected());

    let err = client.recv().await.unwrap_err();
    assert!(matches!(err, KcpError::IoError(_)));
    let err = client.send(b"after close").await.unwrap_err();
    assert!(matches!(err, KcpError::IoError(_)));
}

/// 未建立连接的地址发送应报错而不是挂起
#[tokio::test]
async fn udp_smoke_send_to_unknown_peer_errors() {
    let (mut listener, _addr) = bind_listener().await;
    let ghost: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let err = timeout(SHORT, listener.send_to(b"nobody", ghost)).await.unwrap().unwrap_err();
    assert!(matches!(err, KcpError::IoError(_)));
}

/// 绑定失败应返回错误而不是 panic
#[tokio::test]
async fn udp_smoke_invalid_bind_errors() {
    match KcpListener::bind("256.0.0.1:0").await {
        Ok(_) => panic!("非法地址不应绑定成功"),
        Err(e) => assert!(matches!(e, KcpError::IoError(_))),
    }
}

/// 同一个端口重复绑定应报错
#[tokio::test]
async fn udp_smoke_double_bind_errors() {
    let (_l, addr) = bind_listener().await;
    match KcpListener::bind(addr).await {
        Ok(_) => panic!("端口已被占用不应绑定成功"),
        Err(e) => assert!(matches!(e, KcpError::IoError(_))),
    }
}

/// 自定义流配置下的完整往返
#[tokio::test]
async fn udp_smoke_custom_config_roundtrip() {
    let config = StreamConfig {
        update_interval: Duration::from_millis(2),
        connect_timeout: Duration::from_millis(500),
    };
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", config.clone()).await.unwrap();
    let addr = listener.local_addr();
    let mut client = KcpStream::connect_with_config(addr, config).await.unwrap();

    timeout(SHORT, client.send(b"custom")).await.unwrap().unwrap();
    let (got, peer) = timeout(SHORT, listener.recv()).await.unwrap().unwrap();
    assert_same(&got, b"custom");
    timeout(SHORT, listener.send_to(b"back", peer)).await.unwrap().unwrap();
    assert_same(&timeout(SHORT, client.recv()).await.unwrap().unwrap(), b"back");
}

/// 明文校验 KcpConfig 的各构造器在真实链路里都能工作
#[tokio::test]
async fn udp_smoke_config_constructors() {
    for (tag, cfg) in [
        ("fast", KcpConfig::fast_mode()),
        ("normal", KcpConfig::normal_mode()),
        ("stream", KcpConfig::stream_mode()),
        ("wnd", KcpConfig::fast_mode_with_wnd(64, 128)),
    ] {
        // Stream API 内部固定使用 fast_mode，这里验证构造器本身可用
        let mut kcp = kcp_ovo::core::kcp::Kcp::new(1, cfg).unwrap();
        assert_eq!(kcp.getconv(), 1, "{tag} conv 不一致");
        assert!(kcp.send(b"probe").unwrap() > 0, "{tag} 发送失败");
    }
}
