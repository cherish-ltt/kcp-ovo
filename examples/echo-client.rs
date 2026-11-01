use kcp_ovo::stream::KcpStream;

#[tokio::main]
async fn main() -> kcp_ovo::KcpResult<()> {
    let mut stream = KcpStream::connect("127.0.0.1:8888").await?;
    stream.send(b"Hello, KCP!").await?;
    let reply = stream.recv().await?;
    println!("收到: {}", String::from_utf8_lossy(&reply));
    Ok(())
}
