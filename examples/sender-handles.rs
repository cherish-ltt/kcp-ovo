use std::time::Duration;

use kcp_ovo::stream::{KcpListener, KcpStream};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(10), exchange()).await?
}

async fn exchange() -> Result<(), Box<dyn std::error::Error>> {
    let mut listener = KcpListener::bind("127.0.0.1:0").await?;
    let mut client = KcpStream::connect(listener.local_addr()).await?;
    let client_sender = client.sender();
    let listener_sender = listener.sender();

    let requests = tokio::spawn(async move {
        for sequence in 0_u32..10 {
            client_sender.send(&sequence.to_be_bytes()).await?;
        }
        client_sender.drain(Duration::from_secs(3)).await
    });
    let receiver = tokio::spawn(async move {
        for sequence in 0_u32..10 {
            assert_eq!(client.recv().await?, sequence.to_be_bytes());
        }
        client.drain(Duration::from_secs(3)).await?;
        Ok::<_, kcp_ovo::KcpError>(client)
    });
    let (message, peer) = listener.recv().await?;
    assert_eq!(message, 0_u32.to_be_bytes());
    let responses = tokio::spawn(async move {
        for sequence in 0_u32..10 {
            listener_sender.send_to(&sequence.to_be_bytes(), peer).await?;
        }
        listener_sender.drain_to(peer, Duration::from_secs(3)).await
    });
    for sequence in 1_u32..10 {
        let (message, source) = listener.recv().await?;
        assert_eq!(source, peer);
        assert_eq!(message, sequence.to_be_bytes());
    }
    requests.await??;
    responses.await??;
    let mut client = receiver.await??;
    client.close()?;
    listener.close()?;
    println!("10 条消息已在双端独立收发任务间完成交换与 KCP 确认");
    Ok(())
}
