//! KCP文件传输示例
//!
//! 本示例演示如何使用KCP进行可靠的文件传输
//!
//! 运行方法：
//! 1. 接收端：cargo run --example file-transfer -- recv [output_file]
//! 2. 发送端：cargo run --example file-transfer -- send [input_file]

use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use bytes::Buf;
use kcp_ovo::stream::{KcpListener, KcpStream};
use tokio::time::timeout;

const BUFFER_SIZE: usize = 1400; // KCP的MSS
const PROGRESS_INTERVAL: usize = 100 * 1024; // 每100KB显示进度

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("KCP文件传输示例");
        println!();
        println!("用法:");
        println!("  接收文件: cargo run --example file-transfer -- recv <output_file>");
        println!("  发送文件: cargo run --example file-transfer -- send <input_file>");
        println!();
        println!("示例:");
        println!("  终端1 (接收): cargo run --example file-transfer -- recv received.dat");
        println!("  终端2 (发送): cargo run --example file-transfer -- send /path/to/file.dat");
        return Ok(());
    }

    match args[1].as_str() {
        "recv" => {
            let output_file = if args.len() > 2 {
                &args[2]
            } else {
                "received.dat"
            };
            recv_file(output_file).await?;
        }
        "send" => {
            if args.len() < 3 {
                println!("错误: 请指定要发送的文件");
                return Ok(());
            }
            let input_file = &args[2];
            send_file(input_file).await?;
        }
        _ => {
            println!("未知命令: {}", args[1]);
            println!("请使用 'send' 或 'recv'");
        }
    }

    Ok(())
}

/// 接收文件
async fn recv_file(output_path: &str) -> io::Result<()> {
    println!("KCP文件接收器");
    println!("================");
    println!("监听地址: 0.0.0.0:19999");
    println!("输出文件: {}", output_path);
    println!();

    // 创建监听器
    let mut listener = KcpListener::bind("0.0.0.0:19999").await.unwrap();
    println!("等待发送端连接...");

    // 接收第一个数据包，从中获取发送方地址和至少8字节的元数据
    let (first_data, sender_addr) = listener.recv().await.unwrap();
    println!("已连接到: {}", sender_addr);
    println!();

    // 创建输出文件
    let mut output = File::create(output_path)?;

    // 解析文件大小（前8字节为大端序 u64）
    // 考虑可能粘包：若第一个数据包超过8字节，剩余部分属于文件内容
    let mut buf = &first_data[..];
    let file_size = buf.get_u64();

    let mut total_received: u64 = 0;
    let mut start_time = Instant::now();

    println!(
        "文件大小: {} bytes ({} MB)",
        file_size,
        file_size / 1024 / 1024
    );
    println!();

    // 如果第一个包中还有剩余数据，先写入文件
    if !buf.is_empty() {
        output.write_all(buf)?;
        total_received += buf.len() as u64;
    }

    // 持续接收文件数据
    while total_received < file_size {
        match listener.recv().await {
            Ok((data, addr)) => {
                // 只处理来自同一发送方的数据
                if addr != sender_addr {
                    eprintln!("\n收到来自未知地址 {} 的数据，已忽略", addr);
                    continue;
                }

                output.write_all(&data)?;
                total_received += data.len() as u64;

                // 显示进度
                if total_received % PROGRESS_INTERVAL as u64 == 0 || total_received >= file_size {
                    let progress = (total_received as f64 / file_size as f64) * 100.0;
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let throughput = if elapsed > 0.0 {
                        (total_received as f64 / elapsed) / 1024.0 / 1024.0
                    } else {
                        0.0
                    };
                    print!(
                        "\r进度: {:.1}% | 已接收: {} MB | 速度: {:.2} MB/s",
                        progress,
                        total_received / 1024 / 1024,
                        throughput
                    );
                    io::stdout().flush()?;
                }
            }
            Err(e) => {
                eprintln!("\n接收错误: {}", e);
                return Ok(());
            }
        }
    }

    let elapsed = start_time.elapsed();
    let avg_throughput = if elapsed.as_secs_f64() > 0.0 {
        (total_received as f64 / elapsed.as_secs_f64()) / 1024.0 / 1024.0
    } else {
        0.0
    };

    println!();
    println!("文件接收完成!");
    println!("保存到: {}", output_path);
    println!("总大小: {} bytes", total_received);
    println!("总耗时: {:?}", elapsed);
    println!("平均速度: {:.2} MB/s", avg_throughput);

    Ok(())
}

/// 发送文件
async fn send_file(input_path: &str) -> io::Result<()> {
    println!("KCP文件发送器");
    println!("================");
    println!("目标地址: 127.0.0.1:19999");
    println!("输入文件: {}", input_path);
    println!();

    // 检查文件是否存在
    if !Path::new(input_path).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("文件不存在: {}", input_path),
        ));
    }

    // 打开文件
    let mut input = File::open(input_path)?;
    let file_size = input.metadata()?.len();

    println!(
        "文件大小: {} bytes ({} MB)",
        file_size,
        file_size / 1024 / 1024
    );
    println!();

    // 等待一下让接收端准备就绪
    println!("等待1秒后开始连接...");
    thread::sleep(Duration::from_secs(1));

    // 连接到接收端
    println!("连接到接收端...");
    let data = [0_u8, 1, 2, 3, 4, 5];
    let mut stream = KcpStream::connect("127.0.0.1:19999").await.unwrap();

    println!("已连接");
    println!();

    // 发送文件元数据（大端序 u64）
    let metadata = file_size.to_be_bytes();
    stream.send(&metadata).await.unwrap();

    // 发送文件内容
    let mut buffer = vec![0u8; file_size as usize];
    let mut total_sent = 0u64;
    let mut start_time = Instant::now();

    loop {
        match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                println!("dd{}", n);
                stream.send(&buffer[..n]).await.unwrap();
                let result = stream.recv().await.unwrap();
                total_sent += n as u64;

                // 显示进度
                if total_sent % PROGRESS_INTERVAL as u64 == 0 || total_sent >= file_size {
                    let progress = (total_sent as f64 / file_size as f64) * 100.0;
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let throughput = if elapsed > 0.0 {
                        (total_sent as f64 / elapsed) / 1024.0 / 1024.0
                    } else {
                        0.0
                    };
                    print!(
                        "\r进度: {:.1}% | 已发送: {} MB | 速度: {:.2} MB/s",
                        progress,
                        total_sent / 1024 / 1024,
                        throughput
                    );
                    io::stdout().flush()?;

                    // 小延迟以避免过快发送
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Err(e) => {
                eprintln!("\n读取文件错误: {}", e);
                return Err(e);
            }
        }
    }

    let elapsed = start_time.elapsed();
    let avg_throughput = if elapsed.as_secs_f64() > 0.0 {
        (total_sent as f64 / elapsed.as_secs_f64()) / 1024.0 / 1024.0
    } else {
        0.0
    };

    println!();
    println!("文件发送完成!");
    println!("总大小: {} bytes", total_sent);
    println!("总耗时: {:?}", elapsed);
    println!("平均速度: {:.2} MB/s", avg_throughput);
    let _ = timeout(Duration::from_secs(3), stream.recv()).await;

    Ok(())
}

/// 计算校验和（示例保留，未使用）
fn calculate_checksum(data: &[u8]) -> u32 {
    let mut checksum: u32 = 0;
    for (i, &byte) in data.iter().enumerate() {
        checksum = checksum.wrapping_add(byte as u32);
        checksum = checksum.wrapping_add((i as u32).wrapping_mul(byte as u32));
    }
    checksum
}
