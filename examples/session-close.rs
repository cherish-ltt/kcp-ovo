use std::time::Duration;

use kcp_ovo::KcpError;
use kcp_ovo::stream::KcpListener;

#[tokio::main]
async fn main() -> kcp_ovo::KcpResult<()> {
    let mut listener = KcpListener::bind("127.0.0.1:8888").await?;
    loop {
        let (message, peer) = listener.recv().await?;
        listener.send_to(&message, peer).await?;
        match listener.drain_to(peer, Duration::from_secs(2)).await {
            Ok(()) => println!("{peer}: KCP 确认完成，不代表对端应用已处理"),
            Err(KcpError::TimedOut) => {
                eprintln!("{peer}: 确认超时，数据可能已送达；本示例选择回收会话");
            },
            Err(error) => eprintln!("{peer}: {error}；本示例选择回收会话"),
        }
        println!("{peer}: 已回收 = {}", listener.close_session(peer).await?);
    }
}
