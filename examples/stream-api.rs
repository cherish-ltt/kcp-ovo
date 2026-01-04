//! KCP Stream API 使用示例
//!
//! 本示例展示了如何使用高级Stream API进行KCP通信
//!
//! 运行方法：
//! 1. 先启动服务端：cargo run --example stream-api -- server
//! 2. 再启动客户端：cargo run --example stream-api -- client

use kcp_ovo::{KcpListener, KcpStream, StreamConfig};
use std::env;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("用法:");
        println!("  服务端: cargo run --example stream-api -- server");
        println!("  客户端: cargo run --example stream-api -- client");
        return Ok(());
    }

    match args[1].as_str() {
        "server" => run_server()?,
        "client" => run_client()?,
        _ => {
            println!("未知参数: {}", args[1]);
            println!("请使用 'server' 或 'client'");
        }
    }

    Ok(())
}

/// 运行服务端
fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    println!("KCP Echo服务器启动中...");

    // 创建自定义配置
    let config = StreamConfig {
        update_interval: Duration::from_millis(10),
        recv_buffer_size: 65536,
        auto_update: true,
        connect_timeout: Duration::from_secs(5),
    };

    // 绑定到指定地址
    let mut listener = KcpListener::bind_with_config("0.0.0.0:8888", config)?;

    println!("服务器监听在: 0.0.0.0:8888");
    println!("等待客户端连接...");

    // 接受连接
    let (mut stream, addr) = listener.accept()?;
    println!("客户端 {} 已连接", addr);

    let mut buffer = [0u8; 1024];

    // Echo循环
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("客户端断开连接");
                break;
            }
            Ok(n) => {
                println!(
                    "接收到 {} 字节: {:?}",
                    n,
                    String::from_utf8_lossy(&buffer[..n])
                );

                // Echo回客户端
                stream.write_all(&buffer[..n])?;
                println!("已发送回 {} 字节", n);
            }
            Err(e) => {
                eprintln!("接收错误: {}", e);
                break;
            }
        }
    }

    Ok(())
}

/// 运行客户端
fn run_client() -> Result<(), Box<dyn std::error::Error>> {
    println!("连接到KCP服务器...");

    // 创建自定义配置
    let config = StreamConfig {
        update_interval: Duration::from_millis(10),
        recv_buffer_size: 65536,
        auto_update: true,
        connect_timeout: Duration::from_secs(5),
    };

    // 连接到服务器
    let mut stream = KcpStream::connect_with_config("127.0.0.1:8888", config)?;
    println!("已连接到服务器");

    // 发送测试消息
    let messages = vec![
        "Hello, KCP!",
        "This is a test message",
        "Stream API is easy to use",
        "Testing message 4",
        "Final message - goodbye!",
    ];

    for msg in messages {
        println!("发送: {}", msg);
        stream.write_all(msg.as_bytes())?;

        // 接收响应
        let mut buffer = [0u8; 1024];
        match stream.read(&mut buffer) {
            Ok(n) => {
                println!("收到响应: {}", String::from_utf8_lossy(&buffer[..n]));
            }
            Err(e) => {
                eprintln!("接收响应错误: {}", e);
            }
        }

        thread::sleep(Duration::from_millis(500));
    }

    println!("客户端结束");

    Ok(())
}
