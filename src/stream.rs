//! KCP Stream API - 高级UDP传输层封装
//!
//! 本模块提供了类似TCP的Stream API，自动处理UDP数据包收发和KCP更新。
//!
//! 使用异步 `send` / `recv` 交换消息；服务端通过 `recv` / `send_to`
//! 按来源地址收发。该 API 不实现标准 `Read` / `Write`，也不提供 `accept`。
//!
//! 完整客户端和服务端见仓库中的 `examples/echo-client.rs` 与 `examples/echo-server.rs`。

use bytes::Bytes;
use dashmap::DashMap;
use log::{debug, error, warn};
use tokio::net::{ToSocketAddrs, UdpSocket, lookup_host};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, interval, timeout};

use crate::core::kcp::Kcp;
use crate::error::IncompleteDataType;
use crate::helper::{KcpPacketHeader, current_millis, generate_conv};
use crate::{KcpConfig, KcpError, KcpResult};
use std::io::{self};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

mod drain;

use drain::{SendCommand, drain, finish_drain};

pub const CHANNEL_SIZE: usize = 64;
pub const BUFFER_SIZE: usize = u16::MAX as usize;

fn message_kcp(conv: u32, config: KcpConfig) -> KcpResult<Kcp> {
    if config.stream_mode {
        return Err(KcpError::InvalidConfig("high-level API requires message mode".to_string()));
    }
    // IPv4 的最小头部和 UDP 头部占 28 字节；两种地址族使用同一可移植上限。
    if config.mtu > 65_507 {
        return Err(KcpError::InvalidConfig("UDP mtu must not exceed 65507 bytes".to_string()));
    }
    Kcp::new(conv, config)
}

async fn send_udp(socket: &UdpSocket, data: &[u8], wait: Duration) -> KcpResult<usize> {
    timeout(wait, socket.send(data))
        .await
        .map_err(|_| KcpError::IoError("UDP output timed out".to_string()))?
        .map_err(KcpError::from)
}

/// Stream配置
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 自动更新间隔
    pub update_interval: Duration,
    /// 连接超时时间
    pub connect_timeout: Duration,
    /// 监听器最多同时保留的会话数。
    pub max_sessions: usize,
    /// 监听器回收未收到有效报文的会话的间隔。
    pub session_idle_timeout: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            update_interval: Duration::from_millis(10),
            connect_timeout: Duration::from_secs(5),
            max_sessions: 1024,
            session_idle_timeout: Duration::from_secs(300),
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
    data_sender: Sender<SendCommand>,
    /// 接收数据channel-receiver
    data_receiver: Receiver<Vec<u8>>,
    /// 远程地址
    remote: SocketAddr,
    /// 是否已连接
    connected: bool,
    mss: usize,
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
    /// ```no_run
    /// # async fn example() -> kcp_ovo::KcpResult<()> {
    /// let stream = kcp_ovo::stream::KcpStream::connect("127.0.0.1:8888").await?;
    /// # Ok(())
    /// # }
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
        Self::connect_with_kcp_config(addr, config, KcpConfig::fast_mode()).await
    }

    /// 使用显式协议配置创建消息连接；原有入口仍使用 `KcpConfig::fast_mode()`。
    ///
    /// `mtu` 必须在 29..=65507 字节内，收发窗口须非零，`stream_mode` 必须为 false。
    /// `config.update_interval` 驱动后台更新，`kcp_config.interval` 控制协议刷新；
    /// 两端不协商参数，单条消息的分片数不得超过对端接收窗口和 255。
    ///
    /// ```no_run
    /// # async fn example() -> kcp_ovo::KcpResult<()> {
    /// use kcp_ovo::{KcpConfig, stream::{KcpStream, StreamConfig}};
    /// let protocol = KcpConfig { mtu: 1200, ..KcpConfig::fast_mode_with_wnd(64, 128) };
    /// let mut stream = KcpStream::connect_with_kcp_config(
    ///     "127.0.0.1:8888", StreamConfig::default(), protocol,
    /// ).await?;
    /// stream.send(b"hello").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_with_kcp_config<A: ToSocketAddrs>(
        addr: A,
        config: StreamConfig,
        kcp_config: KcpConfig,
    ) -> KcpResult<Self> {
        if config.update_interval.is_zero() {
            return Err(KcpError::InvalidConfig("update_interval must be non-zero".to_string()));
        }
        let mut kcp = message_kcp(generate_conv(), kcp_config)?;
        let mss = kcp.mss as usize;
        // 解析地址
        let remote = lookup_host(addr).await?.next().ok_or(KcpError::NoAddress)?;

        // 创建UDP socket
        let local = if remote.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let socket = UdpSocket::bind(local).await?;
        socket.connect(remote).await?;
        // 设置output回调
        let socket = Arc::new(socket);
        let socket_clone = socket.clone();
        let timeout_duration = config.connect_timeout;
        kcp.set_output(move |data| {
            let socket_clone = socket_clone.clone();
            async move { send_udp(&socket_clone, &data, timeout_duration).await }
        });

        let (data_tx, mut data_rx) = mpsc::channel::<SendCommand>(CHANNEL_SIZE);
        let (recv_tx, recv_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_SIZE);
        let main_handle = tokio::spawn(async move {
            let mut interval = interval(config.update_interval);
            let mut buf = vec![0; BUFFER_SIZE];
            let mut pending = None;
            loop {
                finish_drain(&kcp, &mut pending);
                let readable = match kcp.peeksize() {
                    Ok(_) => true,
                    Err(KcpError::IncompleteData(IncompleteDataType::PayloadErr)) => {
                        if let Err(e) = kcp.recv() {
                            error!("kcp recv err:{}", e);
                        }
                        continue;
                    },
                    Err(_) => false,
                };
                tokio::select! {
                    permit = recv_tx.reserve(), if readable => {
                        let Ok(permit) = permit else { break };
                        match kcp.recv() {
                            Ok(bytes) => { permit.send(bytes.to_vec()); }
                            Err(e) => error!("kcp recv err:{}", e),
                        }
                    }
                    _ = interval.tick() => {
                        if let Err(e) = kcp.update(current_millis()).await {
                            error!("kcp update err:{}", e);
                            break;
                        }
                    }
                    result = socket.recv(&mut buf) => {
                        match result {
                            Ok(size) => {
                                if let Err(e) = kcp.input(&buf[..size]) {
                                    error!("kcp input err:{}", e);
                                }
                            }
                            Err(e) => {
                                error!("socket err:{}", e);
                                break;
                            },
                        }
                    }
                    _ = async { pending.as_mut().unwrap().closed().await }, if pending.is_some() => {
                        pending = None;
                    }
                    result = data_rx.recv(), if pending.is_none() => {
                        match result {
                            Some(SendCommand::Data(data)) => {
                                if let Err(e) = kcp.send(&data) {
                                    error!("send data err:{}", e);
                                    break;
                                }
                            }
                            Some(SendCommand::Drain(reply)) => pending = Some(reply),
                            None => break,
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
            mss,
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
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }
        Kcp::validate_send_size(data.len(), self.mss, false)?;

        // 发送数据（放入队列）
        if let Err(e) = self.data_sender.send(SendCommand::Data(data.into())).await {
            error!("stream send err: {}", e);
            return Err(KcpError::IoError(format!("stream send err: {}", e)));
        }

        Ok(())
    }

    /// 等待此前成功入队的数据获得 KCP 确认，并输出当前待发 ACK 与窗口通告。
    ///
    /// 总期限包含等待控制请求入队的时间，从 future 首次轮询开始计算。
    /// 成功不代表对端应用已读取或持久化；超时不代表数据未到达。
    /// 取消或超时不撤回已接收的数据，close/Drop 仍立即终止后台任务。
    /// future 绑定当前连接，可在等待期间继续 recv 或显式 close。
    pub fn drain(&self, wait: Duration) -> impl Future<Output = KcpResult<()>> + Send + 'static {
        let sender = if self.is_connected() {
            Ok(self.data_sender.clone())
        } else {
            Err(KcpError::IoError("stream closed".to_string()))
        };
        drain(sender, wait)
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
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 接收数据
        match self.data_receiver.recv().await {
            Some(data) => Ok(data),
            None => Err(KcpError::IoError("stream worker stopped".to_string())),
        }
    }

    /// 尝试发送数据（非阻塞）
    pub fn try_send(&mut self, data: &[u8]) -> KcpResult<()> {
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }
        Kcp::validate_send_size(data.len(), self.mss, false)?;
        self.data_sender
            .try_send(SendCommand::Data(data.into()))
            .map_err(|e| KcpError::IoError(format!("stream try send err:{}", e)))
    }

    /// 尝试接收数据（非阻塞）
    pub fn try_recv(&mut self) -> KcpResult<Vec<u8>> {
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }
        self.data_receiver
            .try_recv()
            .map_err(|e| KcpError::IoError(format!("stream try recv err:{}", e)))
    }

    /// 检查连接状态
    pub fn is_connected(&self) -> bool {
        self.connected && !self.main_handle.is_finished()
    }

    /// 获取远程地址
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }

    /// 关闭连接，丢弃待发送及未读取数据；不会等待对端确认。
    pub fn close(&mut self) -> KcpResult<()> {
        self.connected = false;
        self.main_handle.abort();
        self.data_receiver.close();
        Ok(())
    }
}

impl Drop for KcpStream {
    fn drop(&mut self) {
        self.main_handle.abort();
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
    send_tx: Sender<SendCommand>,
    conv: u32,
    id: u64,
    last_active: Instant,
}

impl Drop for Clinet {
    fn drop(&mut self) {
        self.main_handle.abort();
    }
}

// 主任务取消后仍要清理取消前最后一次插入的会话。
struct ClientCleanup(Arc<DashMap<SocketAddr, Clinet>>);

impl Drop for ClientCleanup {
    fn drop(&mut self) {
        self.0.clear();
    }
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
    recv_rx: Receiver<(Vec<u8>, SocketAddr)>,
    /// 实际绑定的本地地址（绑定 0 端口时用于获取系统分配的端口）
    local_addr: SocketAddr,
    closed: bool,
    mss: usize,
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
    /// ```no_run
    /// # async fn example() -> kcp_ovo::KcpResult<()> {
    /// let listener = kcp_ovo::stream::KcpListener::bind("0.0.0.0:8888").await?;
    /// # Ok(())
    /// # }
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
        Self::bind_with_kcp_config(addr, config, KcpConfig::fast_mode()).await
    }

    /// 使用显式协议配置监听，所有新会话共用该配置。
    ///
    /// 配置约束与 [`KcpStream::connect_with_kcp_config`] 相同，非法配置在绑定前报错。
    pub async fn bind_with_kcp_config<A: ToSocketAddrs>(
        addr: A,
        config: StreamConfig,
        kcp_config: KcpConfig,
    ) -> KcpResult<Self> {
        if config.update_interval.is_zero() {
            return Err(KcpError::InvalidConfig("update_interval must be non-zero".to_string()));
        }
        if config.max_sessions == 0 || config.session_idle_timeout.is_zero() {
            return Err(KcpError::InvalidConfig("session limits must be non-zero".to_string()));
        }
        let mss = message_kcp(0, kcp_config.clone())?.mss as usize;
        let socket = UdpSocket::bind(addr).await.map_err(|e| KcpError::IoError(e.to_string()))?;
        let local_addr = socket.local_addr().map_err(|e| KcpError::IoError(e.to_string()))?;

        let clients: Arc<DashMap<SocketAddr, Clinet>> = Arc::new(DashMap::new());
        let clients_clone = clients.clone();
        let cleanup = ClientCleanup(clients.clone());
        // 设置output回调
        let socket = Arc::new(socket);
        let (udp_tx, mut udp_rx) =
            mpsc::channel::<(Vec<u8>, SocketAddr, u64, oneshot::Sender<()>)>(CHANNEL_SIZE);
        let (recv_tx, recv_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(CHANNEL_SIZE);
        let timeout_druation = Duration::from_millis(100);
        let max_sessions = config.max_sessions;
        let idle_timeout = config.session_idle_timeout;
        let main_handle: JoinHandle<()> = tokio::spawn(async move {
            let _cleanup = cleanup;
            let mut buf = vec![0; BUFFER_SIZE];
            let mut next_id = 0u64;
            let mut cleanup_interval = interval(idle_timeout.min(Duration::from_secs(1)));
            let clients_clone = clients_clone.clone();
            let udp_tx_clone = udp_tx.clone();
            let recv_tx_clone = recv_tx.clone();
            loop {
                let udp_tx_clone = udp_tx_clone.clone();
                let recv_tx_clone = recv_tx_clone.clone();
                tokio::select! {
                    _ = cleanup_interval.tick() => {
                        clients_clone.retain(|_, client| {
                            !client.main_handle.is_finished() && client.last_active.elapsed() < idle_timeout
                        });
                    }
                    result = udp_rx.recv() => {
                        match result {
                            Some((data, addr, id, sent)) => {
                                if !clients_clone.get(&addr).is_some_and(|client| client.id == id) {
                                    continue;
                                }
                                match timeout(timeout_druation, socket.send_to(&data, addr)).await {
                                    Ok(Ok(_)) => { let _ = sent.send(()); },
                                    result => {
                                        error!("socket.send_to err: {:?}", result);
                                        clients_clone.remove_if(&addr, |_, client| client.id == id);
                                    },
                                }
                            }
                            None => warn!("udp_rx.recv none"),
                        }
                    }
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((size, addr)) => {
                                let mut packet = Bytes::copy_from_slice(&buf[..size]);
                                let Ok((header, payload)) = KcpPacketHeader::from_bytes(&mut packet) else { continue };
                                if payload.len() != header.len as usize { continue; }
                                match clients_clone.get_mut(&addr) {
                                    Some(mut client) => {
                                        if client.conv != header.conv { continue; }
                                        client.last_active = Instant::now();
                                        let data = Bytes::copy_from_slice(&buf[..size]).to_vec();
                                        let sender = client.data_tx.clone();
                                        if let Err(e) = sender.try_send(data) {
                                            warn!("listener packet queue: {}", e);
                                        }
                                    }
                                    None => {
                                        if clients_clone.len() >= max_sessions { continue; }
                                        let id = next_id;
                                        next_id = next_id.wrapping_add(1);
                                        // 新链接进入
                                        let Ok(mut kcp) = Kcp::new(0, kcp_config.clone()) else {
                                            error!("Kcp create failed");
                                            continue;
                                        };
                                        let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_SIZE);
                                        let (send_tx, mut send_rx) = mpsc::channel::<SendCommand>(CHANNEL_SIZE);

                                        kcp.set_output(move |data| {
                                            let udp_tx_clone = udp_tx_clone.clone();
                                            async move {
                                                let len = data.len();
                                                let (sent, completed) = oneshot::channel();
                                                udp_tx_clone.send((data.to_vec(), addr, id, sent))
                                                    .await
                                                    .map_err(|_e| KcpError::IoError("send data fail".to_string()))?;
                                                completed.await.map_err(|_| KcpError::IoError("UDP output stopped".to_string()))?;
                                                Ok(len)
                                            }
                                        });
                                        let main_handle = tokio::spawn(async move {
                                            let mut interval = interval(config.update_interval);
                                            let mut pending = None;
                                            loop{
                                                finish_drain(&kcp, &mut pending);
                                                let readable = match kcp.peeksize() {
                                                    Ok(_) => true,
                                                    Err(KcpError::IncompleteData(IncompleteDataType::PayloadErr)) => {
                                                        if let Err(e) = kcp.recv() {
                                                            error!("listener kcp recv err:{}", e);
                                                        }
                                                        continue;
                                                    }
                                                    Err(_) => false,
                                                };
                                                tokio::select! {
                                                    permit = recv_tx_clone.reserve(), if readable => {
                                                        let Ok(permit) = permit else { break };
                                                        match kcp.recv() {
                                                            Ok(bytes) => { permit.send((bytes.to_vec(), addr)); }
                                                            Err(e) => error!("listener kcp recv err:{}", e),
                                                        }
                                                    }
                                                    _ = interval.tick() => {
                                                        if let Err(e) = kcp.update(current_millis()).await {
                                                            error!("kcp update err:{}", e);
                                                            break;
                                                        }
                                                    }
                                                    result = data_rx.recv() => {
                                                        match result {
                                                            Some(data) => {
                                                                match kcp.input(&data) {
                                                                    Ok(_) => debug!("listener kcp input data success"),
                                                                    Err(e) => error!("listener kcp input data err:{}", e),
                                                                }
                                                            }
                                                            None => break,
                                                        }
                                                    }
                                                    _ = async { pending.as_mut().unwrap().closed().await }, if pending.is_some() => {
                                                        pending = None;
                                                    }
                                                    result = send_rx.recv(), if pending.is_none() => {
                                                        match result {
                                                            Some(SendCommand::Data(data)) => {
                                                                if let Err(e) = kcp.send(&data) {
                                                                    error!("listener send data err:{}", e);
                                                                    break;
                                                                }
                                                            }
                                                            Some(SendCommand::Drain(reply)) => pending = Some(reply),
                                                            None => break,
                                                        }
                                                    }
                                                }
                                            }
                                        });

                                        let data = Bytes::copy_from_slice(&buf[..size]).to_vec();
                                        let sender = data_tx.clone();
                                        if let Err(e) = sender.try_send(data) {
                                            warn!("listener packet queue: {}", e);
                                        }

                                        clients_clone.insert(
                                            addr,
                                            Clinet {
                                                addr,
                                                main_handle,
                                                data_tx,
                                                send_tx,
                                                conv: header.conv,
                                                id,
                                                last_active: Instant::now(),
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

        Ok(Self { clients, main_handle, recv_rx, config, local_addr, closed: false, mss })
    }

    /// 获取监听器实际绑定的本地地址
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 关闭监听器和所有会话，丢弃待发送及未读取数据；不会等待对端确认。
    pub fn close(&mut self) -> KcpResult<()> {
        self.closed = true;
        self.main_handle.abort();
        self.clients.clear();
        self.recv_rx.close();
        Ok(())
    }

    /// send
    pub async fn send_to(&mut self, data: &[u8], addr: SocketAddr) -> KcpResult<()> {
        if self.closed {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Listener closed").into());
        }
        let sender = self
            .clients
            .get(&addr)
            .map(|client| client.send_tx.clone())
            .ok_or_else(|| KcpError::IoError("listener send_to err: None".to_string()))?;
        Kcp::validate_send_size(data.len(), self.mss, false)?;
        sender
            .send(SendCommand::Data(data.to_vec()))
            .await
            .map_err(|e| KcpError::IoError(format!("listener send_to err: {}", e)))?;
        Ok(())
    }

    /// 等待指定会话此前入队的数据获得 KCP 确认，并输出当前待发 ACK 与窗口通告。
    ///
    /// 期限、取消及成功语义与 [`KcpStream::drain`] 相同。future 在调用时绑定
    /// 当前会话；同地址的新会话不能完成旧等待。其他会话仍可独立收发。
    pub fn drain_to(
        &self,
        addr: SocketAddr,
        wait: Duration,
    ) -> impl Future<Output = KcpResult<()>> + Send + 'static {
        let sender = if self.closed || self.main_handle.is_finished() {
            None
        } else {
            self.clients.get(&addr).map(|client| client.send_tx.clone())
        };
        drain(sender.ok_or_else(|| KcpError::IoError("session unavailable".to_string())), wait)
    }

    /// recv
    pub async fn recv(&mut self) -> KcpResult<(Vec<u8>, SocketAddr)> {
        if self.closed {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Listener closed").into());
        }
        match self.recv_rx.recv().await {
            Some(result) => Ok(result),
            None => Err(KcpError::IoError("listener send_to err: None".to_string())),
        }
    }
}

impl Drop for KcpListener {
    fn drop(&mut self) {
        self.main_handle.abort();
        self.clients.clear();
    }
}

#[cfg(test)]
mod test {
    use tokio::time::sleep;

    use super::*;

    #[tokio::test]
    async fn udp_output_preserves_socket_error() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(peer.local_addr().unwrap()).await.unwrap();
        let result = send_udp(&socket, &vec![0; 65_508], Duration::from_secs(1)).await;
        assert!(matches!(result, Err(KcpError::IoError(_))), "{result:?}");
    }

    #[tokio::test]
    async fn stopped_stream_worker_wakes_confirmation_waiter() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut stream = KcpStream::connect(peer.local_addr().unwrap()).await.unwrap();
        stream.send(b"unconfirmed").await.unwrap();
        let waiting = tokio::spawn(stream.drain(Duration::from_secs(60)));
        let mut buf = [0; 1500];
        peer.recv(&mut buf).await.unwrap();
        stream.main_handle.abort();
        assert!(matches!(
            timeout(Duration::from_secs(1), waiting).await.unwrap().unwrap(),
            Err(KcpError::IoError(_))
        ));
    }

    #[tokio::test]
    async fn stopped_worker_reports_disconnection() {
        let mut waiting = KcpStream::connect("127.0.0.1:9").await.unwrap();
        let abort = waiting.main_handle.abort_handle();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            abort.abort();
        });
        assert!(timeout(Duration::from_secs(1), waiting.recv()).await.unwrap().is_err());

        let mut stream = KcpStream::connect("127.0.0.1:9").await.unwrap();
        stream.main_handle.abort();
        timeout(Duration::from_secs(1), async {
            while !stream.main_handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!stream.is_connected());
        assert!(stream.recv().await.is_err());
        assert!(stream.send(b"x").await.is_err());
    }

    #[tokio::test(start_paused = false)]
    async fn test_stream() {
        let data1 = [0_u8, 1, 2, 3, 4, 5];
        let data2 = [5_u8, 6, 7, 8, 9, 10];

        let (done, completed) = tokio::sync::oneshot::channel::<()>();
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
            timeout(Duration::from_secs(5), completed).await.unwrap().unwrap();
        });

        sleep(Duration::from_secs(1)).await;
        let mut stream = KcpStream::connect("127.0.0.1:19999").await.unwrap();
        let _ = stream.send(&data1).await;
        let _ = stream.send(&data2).await;
        let result = stream.recv().await.unwrap();
        assert_eq!(result, vec![5, 4, 3, 2, 1, 0]);
        let result = stream.recv().await.unwrap();
        assert_eq!(result, vec![10, 9, 8, 7, 6, 5]);
        done.send(()).unwrap();
        let _ = handle.await;
    }
}
