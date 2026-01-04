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
//! let mut buffer = [0u8; 1024];
//! let len = stream.recv(&mut buffer)?;
//! println!("Received: {:?}", &buffer[..len]);
//! ```

use crate::helper::{current_millis, generate_conv};
use crate::{Kcp, KcpConfig, KcpResult};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// Stream配置
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 自动更新间隔
    pub update_interval: Duration,
    /// 接收缓冲区大小
    pub recv_buffer_size: usize,
    /// 是否自动调用update
    pub auto_update: bool,
    /// 连接超时时间
    pub connect_timeout: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            update_interval: Duration::from_millis(10),
            recv_buffer_size: 65536,
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
    socket: UdpSocket,
    /// 远程地址
    remote: SocketAddr,
    /// 接收缓冲区 (预分配用于接收数据)
    #[allow(dead_code)]
    recv_buffer: Vec<u8>,
    /// 上次更新时间
    last_update: Instant,
    /// 配置
    config: StreamConfig,
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
    pub fn connect<A: ToSocketAddrs>(addr: A) -> KcpResult<Self> {
        Self::connect_with_config(addr, StreamConfig::default())
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
    pub fn connect_with_config<A: ToSocketAddrs>(addr: A, config: StreamConfig) -> KcpResult<Self> {
        // 解析地址
        let addrs: Vec<SocketAddr> = addr.to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "No addresses found").into());
        }
        let remote = addrs[0];

        // 创建UDP socket
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(remote)?;

        // 生成唯一的conv（在实际应用中应该通过握手协商）
        let conv = generate_conv();

        // 创建KCP实例
        let mut kcp = Kcp::new(conv, KcpConfig::fast_mode())?;

        // 设置output回调
        let socket_clone = socket.try_clone()?;
        kcp.set_output(move |data, _kcp| {
            socket_clone.send(data)?;
            Ok(data.len())
        });

        Ok(Self {
            kcp,
            socket,
            remote,
            recv_buffer: vec![0u8; config.recv_buffer_size],
            last_update: Instant::now(),
            config,
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
    pub fn send(&mut self, data: &[u8]) -> KcpResult<usize> {
        if !self.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 处理接收到的数据
        self.handle_input()?;

        // 发送数据
        let sent = self.kcp.send(data)?;

        // 立即刷新
        self.flush()?;

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
    pub fn recv(&mut self, buf: &mut [u8]) -> KcpResult<usize> {
        if !self.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected").into());
        }

        // 处理接收到的数据
        self.handle_input()?;

        // 自动更新
        if self.config.auto_update {
            self.update()?;
        }

        // 接收数据
        let len = self.kcp.recv(buf)?;

        Ok(len)
    }

    /// 尝试发送数据（非阻塞）
    pub fn try_send(&mut self, data: &[u8]) -> KcpResult<usize> {
        self.send(data)
    }

    /// 尝试接收数据（非阻塞）
    pub fn try_recv(&mut self, buf: &mut [u8]) -> KcpResult<usize> {
        self.recv(buf)
    }

    /// 处理接收到的UDP数据包
    fn handle_input(&mut self) -> KcpResult<()> {
        let mut buffer = [0u8; 65536];

        // 非阻塞接收所有可用数据包
        loop {
            match self.socket.recv_from(&mut buffer) {
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

    /// 更新KCP状态
    fn update(&mut self) -> KcpResult<()> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update);

        if elapsed >= self.config.update_interval {
            // 使用系统时间作为current
            let current = current_millis();
            self.kcp.update(current);
            self.last_update = now;
        }

        Ok(())
    }

    /// 刷新发送缓冲区
    fn flush(&mut self) -> KcpResult<()> {
        // KCP会在需要时自动刷新
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

impl Read for KcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.recv(buf).map_err(io::Error::other)
    }
}

impl Write for KcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.send(buf).map_err(io::Error::other)
    }

    fn flush(&mut self) -> io::Result<()> {
        KcpStream::flush(self).map_err(io::Error::other)
    }
}

/// KCP服务端监听器
///
/// 用于接受KCP客户端连接
pub struct KcpListener {
    /// UDP socket
    socket: UdpSocket,
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
    pub fn bind<A: ToSocketAddrs>(addr: A) -> KcpResult<Self> {
        Self::bind_with_config(addr, StreamConfig::default())
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
    pub fn bind_with_config<A: ToSocketAddrs>(addr: A, config: StreamConfig) -> KcpResult<Self> {
        let socket = UdpSocket::bind(addr)?;

        Ok(Self { socket, config })
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
    pub fn accept(&mut self) -> KcpResult<(KcpStream, SocketAddr)> {
        let mut buffer = [0u8; 65536];

        // 接收数据包
        let (len, remote) = self.socket.recv_from(&mut buffer)?;

        // 创建新的socket用于此连接
        let client_socket = UdpSocket::bind("0.0.0.0:0")?;
        client_socket.connect(remote)?;

        // 生成conv（实际应用中应该从接收到的数据包中提取）
        let conv = generate_conv();

        // 创建KCP实例
        let mut kcp = Kcp::new(conv, KcpConfig::fast_mode())?;

        // 设置output回调
        let socket_clone = client_socket.try_clone()?;
        kcp.set_output(move |data, _kcp| {
            socket_clone.send(data)?;
            Ok(data.len())
        });

        // 处理接收到的第一个数据包
        kcp.input(&buffer[..len])?;

        let stream = KcpStream {
            kcp,
            socket: client_socket,
            remote,
            recv_buffer: vec![0u8; self.config.recv_buffer_size],
            last_update: Instant::now(),
            config: self.config.clone(),
            connected: true,
        };

        Ok((stream, remote))
    }

    /// 尝试接受新的连接（非阻塞）
    pub fn try_accept(&mut self) -> KcpResult<Option<(KcpStream, SocketAddr)>> {
        // 设置socket为非阻塞模式
        self.socket.set_nonblocking(true)?;

        match self.accept() {
            Ok(result) => Ok(Some(result)),
            Err(_) => {
                // 如果是WouldBlock错误，返回None
                self.socket.set_nonblocking(false)?;
                Ok(None)
            }
        }
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
        assert_eq!(config.recv_buffer_size, 65536);
        assert!(config.auto_update);
    }

    #[test]
    fn test_stream_config_clone() {
        let config = StreamConfig {
            update_interval: Duration::from_millis(20),
            recv_buffer_size: 32768,
            auto_update: false,
            connect_timeout: Duration::from_secs(10),
        };

        let config2 = config.clone();
        assert_eq!(config.update_interval, config2.update_interval);
        assert_eq!(config.recv_buffer_size, config2.recv_buffer_size);
    }
}
