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
use tokio::net::{ToSocketAddrs, UdpSocket, lookup_host};

use crate::core::kcp::Kcp;
use crate::helper::{current_millis, generate_conv};
use crate::{KcpConfig, KcpError, KcpResult};
use std::io::{self};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Stream配置
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 自动更新间隔
    pub update_interval: Duration,
    /// 是否自动调用update
    pub auto_update: bool,
    /// 连接超时时间
    pub connect_timeout: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            update_interval: Duration::from_millis(10),
            auto_update: true,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

/// KCP客户端流
///
/// 提供类似TCP的连接和数据传输接口
pub struct KcpStream {
    /// KCP实例
    kcp: Kcp,
    /// UDP socket
    socket: Arc<UdpSocket>,
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
        _config: StreamConfig,
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
        kcp.set_output(move |data| {
            let socket_clone = socket_clone.clone();
            async move {
                socket_clone.send(&data).await?; // socket 必须实现了 async send
                Ok(data.len())
            }
        });

        tokio::spawn(async move{
            
        });

        Ok(Self {
            kcp,
            socket,
            remote,
            connected: true,
        })
    }

    /// 发送数据
    ///
    /// # 参数
    ///
    /// - `data`: 要发送的数据
    ///
    /// # 返回
    ///
    /// 返回发送的字节数
    ///
    /// # 示例
    ///
    /// ```ignore
    /// stream.send(b"Hello")?;
    /// ```
    pub async fn send(&mut self, data: &[u8]) -> KcpResult<usize> {
        if !self.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 处理接收到的数据
        // self.handle_input().await?;

        // 发送数据（放入队列）
        let sent = self.kcp.send(data)?;

        // 立即刷新：重置 ts_flush 使 update() 无条件 flush
        // 确保数据在 send() 返回前通过 output callback 发出
        // self.kcp.ts_flush = 0;
        // self.kcp.update(current_millis()).await?;

        Ok(sent)
    }

    /// 接收数据
    ///
    /// # 参数
    ///
    /// - `buf`: 接收缓冲区
    ///
    /// # 返回
    ///
    /// 返回接收到的字节数
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let mut buffer = [0u8; 1024];
    /// let len = stream.recv(&mut buffer)?;
    /// ```
    pub async fn recv(&mut self) -> KcpResult<Bytes> {
        if !self.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 处理接收到的数据
        self.handle_input().await?;

        // 每次 recv 都刷新：确保 send() 入队的数据及时发出，
        // 同时处理重传、ACK 等定时任务。调用频率由 kcp_io_loop 自然控制。
        // self.kcp.update(current_millis())?;

        // 接收数据
        let buf = self.kcp.recv()?;

        Ok(buf)
    }

    /// 尝试发送数据（阻塞）
    pub async fn try_send(&mut self, data: &[u8]) -> KcpResult<usize> {
        self.send(data).await
    }

    /// 尝试接收数据（非阻塞）
    pub async fn try_recv(&mut self) -> KcpResult<Bytes> {
        self.recv().await
    }

    /// 处理接收到的UDP数据包
    async fn handle_input(&mut self) -> KcpResult<()> {
        let mut buffer = [0u8; u16::MAX as usize];

        loop {
            match self.socket.recv_from(&mut buffer).await {
                Ok((len, _src)) => {
                    self.kcp.input(&buffer[..len])?;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    break;
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }

        Ok(())
    }

    /// 检查连接状态
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 获取远程地址
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }

    /// 获取本地地址
    pub fn local_addr(&self) -> KcpResult<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// 关闭连接
    pub fn close(&mut self) -> KcpResult<()> {
        self.connected = false;
        Ok(())
    }
}

/// KCP服务端监听器
///
/// 用于接受KCP客户端连接
pub struct KcpListener {
    /// UDP socket
    socket: Arc<UdpSocket>,
    /// 配置
    config: StreamConfig,
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
        Ok(Self {
            socket: Arc::new(socket),
            config,
        })
    }

    /// 接受新的连接
    ///
    /// # 返回
    ///
    /// 返回(KcpStream, 远程地址)
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let (mut stream, addr) = listener.accept()?;
    /// println!("Connection from: {}", addr);
    /// ```
    pub async fn accept(&mut self) -> KcpResult<(KcpStream, SocketAddr)> {
        let mut buffer = [0u8; u16::MAX as usize];

        // 接收数据包
        let (len, remote) = self.socket.recv_from(&mut buffer).await?;

        // 创建KCP实例
        let mut kcp = Kcp::new(0, KcpConfig::fast_mode())?;

        // 设置output回调
        let socket_clone = self.socket.clone();
        kcp.set_output(move |data| {
            let socket_clone = socket_clone.clone();
            async move {
                socket_clone.send(&data).await?;
                Ok(data.len())
            }
        });

        // 处理接收到的第一个数据包
        kcp.input(&buffer[..len])?;

        let stream = KcpStream {
            kcp,
            socket: self.socket.clone(),
            remote,
            connected: true,
        };

        Ok((stream, remote))
    }

    /// 获取本地地址
    pub fn local_addr(&self) -> KcpResult<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// 关闭监听器
    pub fn close(&mut self) -> KcpResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_config_default() {
        let config = StreamConfig::default();
        assert_eq!(config.update_interval, Duration::from_millis(10));
        assert!(config.auto_update);
    }

    #[test]
    fn test_stream_config_clone() {
        let config = StreamConfig {
            update_interval: Duration::from_millis(20),
            auto_update: false,
            connect_timeout: Duration::from_secs(10),
        };

        let config2 = config.clone();
        assert_eq!(config.update_interval, config2.update_interval);
    }
}
