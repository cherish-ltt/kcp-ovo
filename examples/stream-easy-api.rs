use std::time::Duration;

use kcp_ovo::stream::{KcpListener, KcpStream};
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    let data = [0_u8, 1, 2, 3, 4, 5];

    let _handle = tokio::spawn(async move {
        let mut listener = KcpListener::bind("0.0.0.0:19999").await.unwrap();
        if let Ok(result) = listener.recv().await {
            assert_eq!(result.0, data);
            let data = [5, 4, 3, 2, 1, 0_u8];
            let _ = listener.send_to(&data, result.1).await;
        }
    });

    sleep(Duration::from_secs(1)).await;
    let mut stream = KcpStream::connect("127.0.0.1:19999").await.unwrap();
    let _ = stream.send(&data).await;
    let result = stream.recv().await.unwrap();
    assert_eq!(result, vec![5, 4, 3, 2, 1, 0]);
}
