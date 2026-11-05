//! KCP 文件传输示例：先启动 `recv <output_file>`，再运行 `send <input_file>`。

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kcp_ovo::stream::{KcpListener, KcpStream};
use tokio::time::{Instant, timeout, timeout_at};

const ADDRESS: &str = "127.0.0.1:19999";
const BIND_ADDRESS: &str = "0.0.0.0:19999";
const WAIT: Duration = Duration::from_secs(10);
const CHUNK: usize = 16 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("recv") if args.len() == 3 => {
            let listener = KcpListener::bind(BIND_ADDRESS).await.map_err(io::Error::other)?;
            println!("等待发送端连接...");
            receive_file(listener, Path::new(&args[2]), WAIT).await?;
        },
        Some("send") if args.len() == 3 => send_file(Path::new(&args[2]), ADDRESS, WAIT).await?,
        _ => return Err("用法: file-transfer <send|recv> <文件>".into()),
    }
    Ok(())
}

struct TemporaryFile(Option<PathBuf>);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

async fn receive_from(
    listener: &mut KcpListener,
    peer: SocketAddr,
    wait: Duration,
) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + wait;
    loop {
        let (data, addr) = timeout_at(deadline, listener.recv())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待对端消息超时"))?
            .map_err(io::Error::other)?;
        if addr == peer {
            return Ok(data);
        }
    }
}

fn confirmation_error(error: kcp_ovo::KcpError) -> io::Error {
    let kind = if error == kcp_ovo::KcpError::TimedOut {
        io::ErrorKind::TimedOut
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, error)
}

async fn receive_file(
    mut listener: KcpListener,
    destination: &Path,
    wait: Duration,
) -> io::Result<()> {
    let (metadata, peer) = listener.recv().await.map_err(io::Error::other)?;
    let size = u64::from_be_bytes(
        metadata
            .as_slice()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "文件元数据必须为 8 字节"))?,
    );
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp_path = destination.with_file_name(format!(
        ".{}.{}.{}.tmp",
        destination.file_name().ok_or_else(|| io::Error::other("无效目标路径"))?.to_string_lossy(),
        std::process::id(),
        id
    ));
    let mut temp = TemporaryFile(None);
    let mut output = OpenOptions::new().write(true).create_new(true).open(&temp_path)?;
    temp.0 = Some(temp_path.clone());
    let mut received = 0u64;
    while received < size {
        let data = receive_from(&mut listener, peer, wait).await?;
        if data.is_empty() || data.len() as u64 > size - received {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "文件长度与元数据不符"));
        }
        output.write_all(&data)?;
        received += data.len() as u64;
        listener.send_to(b"NEXT", peer).await.map_err(io::Error::other)?;
    }
    if receive_from(&mut listener, peer, wait).await? != b"END" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "文件结束标记无效"));
    }
    output.sync_all()?;
    drop(output);
    fs::rename(&temp_path, destination)?;
    temp.0 = None;
    let completion = async {
        listener.send_to(b"OK", peer).await.map_err(io::Error::other)?;
        if receive_from(&mut listener, peer, wait).await? != b"DONE" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的完成确认"));
        }
        listener.send_to(b"BYE", peer).await.map_err(io::Error::other)?;
        listener.drain_to(peer, wait).await.map_err(confirmation_error)
    }
    .await;
    completion.map_err(|error| {
        io::Error::new(error.kind(), format!("文件已保存，但结束确认未完成: {error}"))
    })?;
    println!("文件接收完成: {} bytes", received);
    Ok(())
}

async fn send_file(source: &Path, address: &str, wait: Duration) -> io::Result<()> {
    let mut input = File::open(source)?;
    let size = input.metadata()?.len();
    let mut stream = KcpStream::connect(address.to_owned()).await.map_err(io::Error::other)?;
    stream.send(&size.to_be_bytes()).await.map_err(io::Error::other)?;
    let mut buffer = [0u8; CHUNK];
    let mut sent = 0u64;
    while sent < size {
        let limit = (size - sent).min(CHUNK as u64) as usize;
        let n = input.read(&mut buffer[..limit])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "源文件提前结束"));
        }
        stream.send(&buffer[..n]).await.map_err(io::Error::other)?;
        let result = timeout(wait, stream.recv())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待分块确认超时"))?
            .map_err(io::Error::other)?;
        if result != b"NEXT" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的分块确认"));
        }
        sent += n as u64;
    }
    stream.send(b"END").await.map_err(io::Error::other)?;
    let result = timeout(wait, stream.recv())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待接收端完成超时，文件可能已保存"))?
        .map_err(io::Error::other)?;
    if result != b"OK" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的接收端回复"));
    }
    let completion = async {
        stream.send(b"DONE").await.map_err(io::Error::other)?;
        let result = timeout(wait, stream.recv())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待最终确认超时"))?
            .map_err(io::Error::other)?;
        if result != b"BYE" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的最终确认"));
        }
        stream.drain(wait).await.map_err(confirmation_error)
    }
    .await;
    completion.map_err(|error| {
        io::Error::new(error.kind(), format!("接收端已报告保存，但结束确认未完成: {error}"))
    })?;
    println!("文件发送完成: {} bytes", sent);
    Ok(())
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use kcp_ovo::helper::KcpPacketHeader;

    use super::*;
    use common::udp::UdpRelay;

    #[tokio::test]
    async fn file_roundtrips_and_invalid_input_preserves_destination() {
        let dir = env::temp_dir().join(format!("kcp-file-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        for size in [0, 1024, CHUNK * 2 + 1] {
            let source = dir.join("source");
            let destination = dir.join(format!("output-{size}"));
            let data: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
            fs::write(&source, &data).unwrap();
            let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().to_string();
            let (received, sent) = timeout(Duration::from_secs(8), async {
                tokio::join!(
                    receive_file(listener, &destination, WAIT),
                    send_file(&source, &addr, WAIT)
                )
            })
            .await
            .unwrap();
            received.unwrap();
            sent.unwrap();
            assert_eq!(fs::read(destination).unwrap(), data);
        }
        for (metadata, exact_first) in [
            (vec![1], false),
            (2u64.to_be_bytes().to_vec(), false),
            (2u64.to_be_bytes().to_vec(), true),
        ] {
            let destination = dir.join("existing");
            fs::write(&destination, b"original").unwrap();
            let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr();
            let (done, completed) = tokio::sync::oneshot::channel();
            let sender = async {
                let mut stream = KcpStream::connect(addr).await.unwrap();
                stream.send(&metadata).await.unwrap();
                if exact_first {
                    stream.send(b"ok").await.unwrap();
                    assert_eq!(timeout(WAIT, stream.recv()).await.unwrap().unwrap(), b"NEXT");
                    stream.send(b"extra").await.unwrap();
                } else if metadata.len() == 8 {
                    stream.send(b"too long").await.unwrap();
                }
                completed.await.unwrap();
            };
            let (received, ()) = timeout(Duration::from_secs(8), async {
                tokio::join!(
                    async {
                        let result = receive_file(listener, &destination, WAIT).await;
                        done.send(()).unwrap();
                        result
                    },
                    sender
                )
            })
            .await
            .unwrap();
            assert_eq!(received.unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(destination).unwrap(), b"original");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn final_reply_and_ack_loss_preserve_saved_file() {
        let dir = env::temp_dir().join(format!("kcp-file-final-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source");
        let data: Vec<u8> = (0..CHUNK * 2 + 1).map(|n| (n % 251) as u8).collect();
        fs::write(&source, &data).unwrap();
        for loss in ["first BYE", "all BYE", "final confirmation"] {
            let destination = dir.join(loss);
            let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut final_una = None;
            let relay = UdpRelay::start_with_filter(
                listener.local_addr(),
                common::Net::default().with_delay(1),
                4103,
                move |packet, from_server| {
                    let (header, payload) = KcpPacketHeader::from_bytes(&mut &packet[..]).unwrap();
                    if from_server && payload == b"BYE" {
                        let first = final_una.is_none();
                        final_una = Some(header.sn.wrapping_add(1));
                        return loss == "all BYE" || (loss == "first BYE" && first);
                    }
                    loss == "final confirmation" && !from_server && final_una == Some(header.una)
                },
            )
            .await;
            let addr = relay.addr.to_string();
            let wait = Duration::from_secs(2);
            let (received, sent) = timeout(Duration::from_secs(5), async {
                tokio::join!(
                    receive_file(listener, &destination, wait),
                    send_file(&source, &addr, wait)
                )
            })
            .await
            .unwrap();
            if loss == "first BYE" {
                received.unwrap();
            } else {
                let error = received.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                assert!(error.to_string().contains("文件已保存"));
            }
            if loss == "all BYE" {
                let error = sent.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                assert!(error.to_string().contains("接收端已报告保存"));
            } else {
                sent.unwrap();
            }
            assert_eq!(fs::read(&destination).unwrap(), data);
            assert!(relay.stats.lock().unwrap().filtered > 0, "{loss}");
            relay.stop().await;
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn early_sender_exit_reports_file_state_and_cleans_partial_output() {
        let dir = env::temp_dir().join(format!("kcp-file-exit-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        for committed in [false, true] {
            let destination = dir.join("existing");
            fs::write(&destination, b"original").unwrap();
            let listener = KcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr();
            let sender = async {
                let mut stream = KcpStream::connect(addr).await.unwrap();
                stream.send(&2u64.to_be_bytes()).await.unwrap();
                stream.send(b"ok").await.unwrap();
                assert_eq!(stream.recv().await.unwrap(), b"NEXT");
                if committed {
                    stream.send(b"END").await.unwrap();
                    assert_eq!(stream.recv().await.unwrap(), b"OK");
                }
                stream.drain(Duration::from_secs(2)).await.unwrap();
            };
            let (received, ()) = timeout(Duration::from_secs(5), async {
                tokio::join!(receive_file(listener, &destination, Duration::from_secs(2)), sender)
            })
            .await
            .unwrap();
            let error = received.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string().contains("文件已保存"), committed);
            assert_eq!(
                fs::read(&destination).unwrap(),
                if committed { b"ok".as_slice() } else { b"original".as_slice() }
            );
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "temporary file leaked");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
