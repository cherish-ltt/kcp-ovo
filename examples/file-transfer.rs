//! KCP 文件传输示例：先启动 `recv <output_file>`，再运行 `send <input_file>`。

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kcp_ovo::stream::{KcpListener, KcpStream};
use tokio::time::{Instant, sleep, timeout, timeout_at};

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
            receive_file(listener, Path::new(&args[2])).await?;
        },
        Some("send") if args.len() == 3 => send_file(Path::new(&args[2]), ADDRESS).await?,
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

async fn receive_from(listener: &mut KcpListener, peer: SocketAddr) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + WAIT;
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

async fn receive_file(mut listener: KcpListener, destination: &Path) -> io::Result<()> {
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
        let data = receive_from(&mut listener, peer).await?;
        if data.is_empty() || data.len() as u64 > size - received {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "文件长度与元数据不符"));
        }
        output.write_all(&data)?;
        received += data.len() as u64;
        listener.send_to(b"NEXT", peer).await.map_err(io::Error::other)?;
    }
    if receive_from(&mut listener, peer).await? != b"END" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "文件结束标记无效"));
    }
    output.sync_all()?;
    drop(output);
    fs::rename(&temp_path, destination)?;
    temp.0 = None;
    listener.send_to(b"OK", peer).await.map_err(io::Error::other)?;
    if receive_from(&mut listener, peer).await? != b"DONE" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的完成确认"));
    }
    listener.send_to(b"BYE", peer).await.map_err(io::Error::other)?;
    sleep(Duration::from_secs(1)).await;
    println!("文件接收完成: {} bytes", received);
    Ok(())
}

async fn send_file(source: &Path, address: &str) -> io::Result<()> {
    let mut input = File::open(source)?;
    let size = input.metadata()?.len();
    let mut stream = KcpStream::connect(address).await.map_err(io::Error::other)?;
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
        let result = timeout(WAIT, stream.recv())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待分块确认超时"))?
            .map_err(io::Error::other)?;
        if result != b"NEXT" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的分块确认"));
        }
        sent += n as u64;
    }
    stream.send(b"END").await.map_err(io::Error::other)?;
    let result = timeout(WAIT, stream.recv())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待接收端完成超时"))?
        .map_err(io::Error::other)?;
    if result != b"OK" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的接收端回复"));
    }
    stream.send(b"DONE").await.map_err(io::Error::other)?;
    let result = timeout(WAIT, stream.recv())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "等待最终确认超时"))?
        .map_err(io::Error::other)?;
    if result != b"BYE" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "无效的最终确认"));
    }
    println!("文件发送完成: {} bytes", sent);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                tokio::join!(receive_file(listener, &destination), send_file(&source, &addr))
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
                sleep(Duration::from_millis(200)).await;
            };
            let (received, ()) = timeout(Duration::from_secs(8), async {
                tokio::join!(receive_file(listener, &destination), sender)
            })
            .await
            .unwrap();
            assert_eq!(received.unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(destination).unwrap(), b"original");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
