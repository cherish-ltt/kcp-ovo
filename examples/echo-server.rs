use kcp_ovo::stream::KcpListener;

#[tokio::main]
async fn main() -> kcp_ovo::KcpResult<()> {
    let mut listener = KcpListener::bind("127.0.0.1:8888").await?;
    loop {
        let (data, peer) = listener.recv().await?;
        listener.send_to(&data, peer).await?;
    }
}
