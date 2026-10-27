//! 黑盒测试：真实 UDP 链路上的混沌场景
//!
//! 前面的真实网络用例只覆盖「一切正常」的路径。这里补上部署时真正会遇到的情况：
//! 多客户端并发且消息边界必须保留、监听端口被垃圾报文轰炸后仍能服务新连接、
//! 某个客户端停止读取时不得拖死其它客户端。

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::{Rng, checksum};
use kcp_ovo::stream::{KcpListener, KcpStream, StreamConfig};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const BUDGET: Duration = Duration::from_secs(20);

fn stream_cfg() -> StreamConfig {
    StreamConfig {
        update_interval: Duration::from_millis(5),
        connect_timeout: Duration::from_secs(3),
    }
}

/// 起一个回显服务：把收到的每条消息原样发回给来源地址
async fn echo_server() -> (SocketAddr, JoinHandle<()>) {
    let mut listener = KcpListener::bind_with_config("127.0.0.1:0", stream_cfg()).await.unwrap();
    let addr = listener.local_addr();
    let handle = tokio::spawn(async move {
        while let Ok((data, peer)) = listener.recv().await {
            if listener.send_to(&data, peer).await.is_err() {
                break;
            }
        }
    });
    (addr, handle)
}

async fn connect(addr: SocketAddr) -> KcpStream {
    timeout(BUDGET, KcpStream::connect_with_config(addr, stream_cfg()))
        .await
        .expect("连接超时")
        .expect("连接失败")
}

/// 八个客户端并发收发各自的消息，消息边界与内容都不得串台
#[tokio::test]
async fn eight_clients_keep_message_boundaries() {
    const CLIENTS: usize = 8;
    const ROUNDS: usize = 6;

    let (addr, server) = echo_server().await;
    let mut handles = Vec::new();
    for i in 0..CLIENTS {
        handles.push(tokio::spawn(async move {
            let mut stream = connect(addr).await;
            let mut rng = Rng::new(0x5EED_0000 + i as u64);
            let mut sent = Vec::new();
            for _ in 0..ROUNDS {
                // 消息长度跨越多个分片，但不接近接收窗口上限
                let len = 512 + (rng.next_u64() as usize % 7168);
                let msg = rng.bytes(len);
                stream.send(&msg).await.expect("send");
                sent.push(msg);
            }
            let mut got = Vec::new();
            for _ in 0..ROUNDS {
                got.push(stream.recv().await.expect("recv"));
            }
            (sent, got)
        }));
    }

    for handle in handles {
        let (sent, got) = timeout(BUDGET, handle).await.expect("客户端超时").expect("客户端 panic");
        assert_eq!(got.len(), sent.len(), "回显条数不一致");
        for (round, (g, s)) in got.iter().zip(sent.iter()).enumerate() {
            assert_eq!(g.len(), s.len(), "第 {round} 条消息长度不一致");
            assert_eq!(checksum(g), checksum(s), "第 {round} 条消息内容不一致");
        }
    }
    server.abort();
}

/// 监听端口被垃圾报文轰炸后，真实客户端仍必须能完成连接与收发
#[tokio::test]
async fn garbage_datagrams_do_not_break_listener() {
    let (addr, server) = echo_server().await;
    let attacker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut rng = Rng::new(0xBADB_0001);

    // 混合各种长度的随机字节、截断头与伪造头部
    for i in 0..300u64 {
        let len = match i % 4 {
            0 => 1 + (rng.next_u64() as usize % 27),
            1 => 28,
            2 => 28 + (rng.next_u64() as usize % 64),
            _ => (rng.next_u64() as usize % 600) + 1,
        };
        let mut junk = rng.bytes(len);
        if junk.len() >= 28 {
            // 让部分垃圾报文伪装成合法命令字与伪造 conv
            junk[4] = 81 + (i % 4) as u8;
            junk[0..4].copy_from_slice(&(i as u32).to_be_bytes());
        }
        attacker.send_to(&junk, addr).await.unwrap();
    }

    // 垃圾轰炸之后，真实客户端的三轮收发必须完全正常
    let mut stream = connect(addr).await;
    for round in 0..3u8 {
        let msg = vec![round; 2048];
        timeout(BUDGET, stream.send(&msg)).await.expect("send 超时").expect("send 失败");
        let got = timeout(BUDGET, stream.recv()).await.expect("recv 超时").expect("recv 失败");
        assert_eq!(got, msg, "垃圾报文污染了正常连接");
    }
    server.abort();
}

/// 某个客户端停止读取时，其它客户端不受影响
#[tokio::test]
async fn silent_client_does_not_block_others() {
    let (addr, server) = echo_server().await;

    // 这个客户端发完就再也不读，其接收链路会积压数据
    let mut idle = connect(addr).await;
    timeout(BUDGET, idle.send(&vec![0xEEu8; 4096])).await.expect("send 超时").expect("send 失败");

    // 另一个客户端必须照常完成多轮收发
    let mut active = connect(addr).await;
    for round in 0..5u8 {
        let msg = vec![round; 1024];
        timeout(BUDGET, active.send(&msg)).await.expect("send 超时").expect("send 失败");
        let got = timeout(BUDGET, active.recv()).await.expect("recv 超时").expect("recv 失败");
        assert_eq!(got, msg, "静默客户端拖累了正常客户端");
    }
    server.abort();
}

/// 大消息（约 30 个分片）在真实 UDP 上必须保持消息边界与内容
#[tokio::test]
async fn large_message_roundtrip_over_udp() {
    let (addr, server) = echo_server().await;
    let mut stream = connect(addr).await;

    let payload = Rng::new(0x1A26_0002).bytes(40 * 1024);
    for _ in 0..3 {
        timeout(BUDGET, stream.send(&payload)).await.expect("send 超时").expect("send 失败");
        let got = timeout(BUDGET, stream.recv()).await.expect("recv 超时").expect("recv 失败");
        assert_eq!(got.len(), payload.len(), "大消息长度不一致");
        assert_eq!(checksum(&got), checksum(&payload), "大消息内容不一致");
    }
    server.abort();
}
