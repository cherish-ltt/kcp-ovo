//! KCP Stream API - 高级UDP传输层封装
//!
//! 本模块提供了类似TCP的Stream API，自动处理UDP数据包收发和KCP更新。
//!
//! # 特性
//!
//! - 自动处理KCP更新
//! - 类似TCP的send/recv接口
//! - 实现了std::io::Read和std::io::Write trait
//! - 支持非阻塞操作
//!
//! # 示例
//!
//! ```ignore
//! use kcp_ovo::stream::KcpStream;
//!
//! // 连接到远程服务器
//! let mut stream = KcpStream::connect("127.0.0.1:8888")?;
//!
//! // 发送数据
//! stream.send(b"Hello, KCP!")?;
//!
//! // 接收数据
//! let bytes = stream.recv()?;
//! println!("Received: {:?}", &bytes[..len]);
//! ```

use bytes::Bytes;
use dashmap::DashMap;
use log::{debug, error, warn};
use tokio::net::{ToSocketAddrs, UdpSocket, lookup_host};
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};

use crate::core::kcp::Kcp;
use crate::helper::{current_millis, generate_conv};
use crate::{KcpConfig, KcpError, KcpResult};
use std::io::{self};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub const CHANNEL_SIZE: usize = 64;
pub const BUFFER_SIZE: usize = u16::MAX as usize;

/// Stream配置
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 自动更新间隔
    pub update_interval: Duration,
    /// 连接超时时间
    pub connect_timeout: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            update_interval: Duration::from_millis(10),
            connect_timeout: Duration::from_secs(5),
        }
    }
}

/// KCP客户端流
///
/// 提供类似TCP的连接和数据传输接口
#[allow(dead_code)]
pub struct KcpStream {
    /// 主线程
    main_handle: JoinHandle<()>,
    /// 发送数据channel-sender
    data_sender: Sender<Vec<u8>>,
    /// 接收数据channel-receiver
    data_receiver: Receiver<Vec<u8>>,
    /// 远程地址
    remote: SocketAddr,
    /// 是否已连接
    connected: bool,
}

impl KcpStream {
    /// 连接到远程KCP服务器
    ///
    /// # 参数
    ///
    /// - `addr`: 远程地址，格式为 "IP:PORT"
    ///
    /// # 返回
    ///
    /// 返回连接成功的KcpStream实例
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let stream = KcpStream::connect("127.0.0.1:8888")?;
    /// ```
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> KcpResult<Self> {
        Self::connect_with_config(addr, StreamConfig::default()).await
    }

    /// 使用指定配置连接到远程KCP服务器
    ///
    /// # 参数
    ///
    /// - `addr`: 远程地址
    /// - `config`: Stream配置
    ///
    /// # 返回
    ///
    /// 返回连接成功的KcpStream实例
    pub async fn connect_with_config<A: ToSocketAddrs>(
        addr: A,
        config: StreamConfig,
    ) -> KcpResult<Self> {
        // 解析地址
        let remote = lookup_host(addr).await?.next().ok_or(KcpError::NoAddress)?;

        // 创建UDP socket
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(remote).await?;
        // 生成唯一的conv(客户端生成，需服务端确认)
        let conv = generate_conv();

        // 创建KCP实例
        let mut kcp = Kcp::new(conv, KcpConfig::fast_mode())?;

        // 设置output回调
        let socket = Arc::new(socket);
        let socket_clone = socket.clone();
        let timeout_duration = config.connect_timeout;
        kcp.set_output(move |data| {
            let socket_clone = socket_clone.clone();
            async move {
                let _ = timeout(timeout_duration, socket_clone.send(&data))
                    .await
                    .map_err(|_e| KcpError::IoError("send data fail".to_string()))?;
                Ok(data.len())
            }
        });

        let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_SIZE);
        let (recv_tx, recv_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_SIZE);
        let main_handle = tokio::spawn(async move {
            let mut interval = interval(config.update_interval);
            let mut buf = vec![0; BUFFER_SIZE];
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = kcp.update(current_millis()).await {
                            error!("kcp update err:{}", e);
                        }
                    }
                    result = socket.recv(&mut buf) => {
                        match result {
                            Ok(size) => {
                                if let Err(e) = kcp.input(&buf[..size]) {
                                    error!("kcp input err:{}", e);
                                }
                                match kcp.recv() {
                                    Ok(bytes) => {
                                        if let Err(e) = recv_tx.send(bytes.to_vec()).await {
                                            error!("recv_tx send err:{}", e);
                                        }
                                    }
                                    Err(e) => error!("kcp recv err:{}", e),
                                }
                            }
                            Err(e) => error!("socket err:{}", e),
                        }
                    }
                    result = data_rx.recv() => {
                        match result {
                            Some(data) => {
                                match kcp.send(&data) {
                                    Ok(size) => {debug!("send data(size-{}) success", size)},
                                    Err(e) => error!("send data err:{}", e),
                                }

                            }
                            None => {
                                break;
                            },
                        }
                    }
                }
            }
        });

        Ok(Self {
            main_handle,
            data_sender: data_tx,
            data_receiver: recv_rx,
            remote,
            connected: true,
        })
    }

    /// 发送数据(阻塞)
    ///
    /// # 参数
    ///
    /// - `data`: 要发送的数据
    ///
    /// # 示例
    ///
    /// ```ignore
    /// stream.send(b"Hello")?;
    /// ```
    pub async fn send(&mut self, data: &[u8]) -> KcpResult<()> {
        if !self.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 发送数据（放入队列）
        if let Err(e) = self.data_sender.send(data.into()).await {
            error!("stream send err: {}", e);
            return Err(KcpError::IoError(format!("stream send err: {}", e)));
        }

        Ok(())
    }

    /// 接收数据(阻塞)
    ///
    /// # 参数
    ///
    /// - `buf`: 接收缓冲区
    ///
    /// # 返回
    ///
    /// 返回接收到的Vec<u8>
    pub async fn recv(&mut self) -> KcpResult<Vec<u8>> {
        if !self.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 接收数据
        match self.data_receiver.recv().await {
            Some(data) => Ok(data),
            None => Ok(Vec::new()),
        }
    }

    /// 尝试发送数据（非阻塞）
    pub fn try_send(&mut self, data: &[u8]) -> KcpResult<()> {
        self.data_sender
            .try_send(data.into())
            .map_err(|e| KcpError::IoError(format!("stream try send err:{}", e)))
    }

    /// 尝试接收数据（非阻塞）
    pub fn try_recv(&mut self) -> KcpResult<Vec<u8>> {
        self.data_receiver
            .try_recv()
            .map_err(|e| KcpError::IoError(format!("stream try recv err:{}", e)))
    }

    /// 检查连接状态
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 获取远程地址
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }

    /// 关闭连接
    pub fn close(&mut self) -> KcpResult<()> {
        self.connected = false;
        Ok(())
    }
}

#[allow(dead_code)]
pub struct Clinet {
    /// SocketAddr
    addr: SocketAddr,
    /// 主线程
    main_handle: JoinHandle<()>,
    /// listener-msg
    data_tx: Sender<Vec<u8>>,
    /// send-msg
    send_tx: Sender<Vec<u8>>,
}

/// KCP服务端监听器
///
/// 用于接受KCP客户端连接
#[allow(dead_code)]
pub struct KcpListener {
    /// clients
    clients: Arc<DashMap<SocketAddr, Clinet>>,
    /// 主线程
    main_handle: JoinHandle<()>,
    /// 配置
    config: StreamConfig,
    /// recv-msg
    recv_rx: UnboundedReceiver<(Vec<u8>, SocketAddr)>,
}

impl KcpListener {
    /// 绑定到指定地址并开始监听
    ///
    /// # 参数
    ///
    /// - `addr`: 绑定地址，格式为 "IP:PORT"
    ///
    /// # 返回
    ///
    /// 返回KcpListener实例
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let listener = KcpListener::bind("0.0.0.0:8888")?;
    /// ```
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> KcpResult<Self> {
        Self::bind_with_config(addr, StreamConfig::default()).await
    }

    /// 使用指定配置绑定到指定地址
    ///
    /// # 参数
    ///
    /// - `addr`: 绑定地址
    /// - `config`: Stream配置
    ///
    /// # 返回
    ///
    /// 返回KcpListener实例
    pub async fn bind_with_config<A: ToSocketAddrs>(
        addr: A,
        config: StreamConfig,
    ) -> KcpResult<Self> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|e| KcpError::IoError(e.to_string()))?;

        let clients: Arc<DashMap<SocketAddr, Clinet>> = Arc::new(DashMap::new());
        let clients_clone = clients.clone();
        // 设置output回调
        let socket = Arc::new(socket);
        let (udp_tx, mut udp_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(CHANNEL_SIZE);
        let (recv_tx, recv_rx) = mpsc::unbounded_channel::<(Vec<u8>, SocketAddr)>();
        let timeout_druation = Duration::from_millis(100);
        let main_handle: JoinHandle<()> = tokio::spawn(async move {
            let mut buf = vec![0; BUFFER_SIZE];
            let clients_clone = clients_clone.clone();
            let udp_tx_clone = udp_tx.clone();
            let recv_tx_clone = recv_tx.clone();
            loop {
                let udp_tx_clone = udp_tx_clone.clone();
                let recv_tx_clone = recv_tx_clone.clone();
                tokio::select! {
                    result = udp_rx.recv() => {
                        match result {
                            Some((data, addr)) => {
                                if let Err(e) = timeout(timeout_druation, socket.send_to(&data, addr)).await {
                                    error!("socket.send_to err: {}", e);
                                }
                            }
                            None => warn!("udp_rx.recv none"),
                        }
                    }
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((size, addr)) => {
                                match clients_clone.get(&addr) {
                                    Some(client) => {
                                        let data = Bytes::copy_from_slice(&buf[..size]).to_vec();
                                        let sender = client.data_tx.clone();
                                        tokio::spawn(async move {
                                            let _ = sender.send(data).await;
                                        });
                                    }
                                    None => {
                                        // 新链接进入
                                        let Ok(mut kcp) = Kcp::new(0, KcpConfig::fast_mode()) else {
                                            error!("Kcp create failed");
                                            continue;
                                        };
                                        let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_SIZE);
                                        let (send_tx, mut send_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_SIZE);

                                        kcp.set_output(move |data| {
                                            let udp_tx_clone = udp_tx_clone.clone();
                                            async move {
                                                let len = data.len();
                                                udp_tx_clone.send((data.to_vec(),addr))
                                                    .await
                                                    .map_err(|_e| KcpError::IoError("send data fail".to_string()))?;
                                                Ok(len)
                                            }
                                        });
                                        let main_handle = tokio::spawn(async move {
                                            let mut interval = interval(config.update_interval);
                                            loop{
                                                tokio::select! {
                                                    _ = interval.tick() => {
                                                        if let Err(e) = kcp.update(current_millis()).await {
                                                            error!("kcp update err:{}", e);
                                                        }
                                                    }
                                                    result = data_rx.recv() => {
                                                        match result {
                                                            Some(data) => {
                                                                match kcp.input(&data) {
                                                                    Ok(_) => debug!("listener kcp input data success"),
                                                                    Err(e) => error!("listener kcp input data err:{}", e),
                                                                }
                                                                match kcp.recv() {
                                                                    Ok(bytes) => {
                                                                        if let Err(e) = recv_tx_clone.send((bytes.to_vec(),addr)) {
                                                                            error!("recv_tx send err:{}", e);
                                                                        }
                                                                    }
                                                                    Err(e) => {warn!("66601:{}",e)}
                                                                }
                                                            }
                                                            None => warn!("listener data_rx.recv none"),
                                                        }
                                                    }
                                                    result = send_rx.recv() => {
                                                        match result {
                                                            Some(data) => {
                                                                match kcp.send(&data) {
                                                                    Ok(size) => debug!("listener send data(size-{}) success", size),
                                                                    Err(e) => error!("listener send data err:{}", e),
                                                                }
                                                            }
                                                            None => warn!("listener data_rx.recv none"),
                                                        }
                                                    }
                                                }
                                            }
                                        });

                                        let data = Bytes::copy_from_slice(&buf[..size]).to_vec();
                                        let sender = data_tx.clone();
                                        tokio::spawn(async move {
                                            let _ = sender.send(data).await;
                                        });

                                        clients_clone.insert(
                                            addr,
                                            Clinet {
                                                addr,
                                                main_handle,
                                                data_tx,
                                                send_tx,
                                            },
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                error!("socket.recv_from err: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            clients,
            main_handle,
            recv_rx,
            config,
        })
    }

    /// 关闭监听器
    pub fn close(&mut self) -> KcpResult<()> {
        Ok(())
    }

    /// send
    pub async fn send_to(&mut self, data: &[u8], addr: SocketAddr) -> KcpResult<()> {
        match self.clients.get(&addr) {
            Some(client) => {
                client
                    .send_tx
                    .send(data.to_vec())
                    .await
                    .map_err(|e| KcpError::IoError(format!("listener send_to err: {}", e)))?;
            }
            None => return Err(KcpError::IoError("listener send_to err: None".to_string())),
        }
        Ok(())
    }

    /// recv
    pub async fn recv(&mut self) -> KcpResult<(Vec<u8>, SocketAddr)> {
        match self.recv_rx.recv().await {
            Some(result) => Ok(result),
            None => Err(KcpError::IoError("listener send_to err: None".to_string())),
        }
    }
}

#[cfg(test)]
mod test {
    use tokio::time::sleep;

    use super::*;

    #[tokio::test(start_paused = false)]
    async fn test_stream() {
        let data1 = [0_u8, 1, 2, 3, 4, 5];
        let data2 = [5_u8, 6, 7, 8, 9, 10];

        let handle = tokio::spawn(async move {
            let mut listener = KcpListener::bind("0.0.0.0:19999").await.unwrap();
            if let Ok(result) = listener.recv().await {
                assert_eq!(result.0, data1);
                let data1 = [5, 4, 3, 2, 1, 0_u8];
                let _ = listener.send_to(&data1, result.1).await;
            }
            if let Ok(result) = listener.recv().await {
                assert_eq!(result.0, data2);
                let data2 = [10, 9, 8, 7, 6, 5_u8];
                let _ = listener.send_to(&data2, result.1).await;
            }
        });

        sleep(Duration::from_secs(1)).await;
        let mut stream = KcpStream::connect("127.0.0.1:19999").await.unwrap();
        let _ = stream.send(&data1).await;
        let _ = stream.send(&data2).await;
        let result = stream.recv().await.unwrap();
        assert_eq!(result, vec![5, 4, 3, 2, 1, 0]);
        let result = stream.recv().await.unwrap();
        assert_eq!(result, vec![10, 9, 8, 7, 6, 5]);
        let _ = handle.await;
    }
}
