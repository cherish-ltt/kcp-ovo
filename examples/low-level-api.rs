//! KCP底层API使用示例
//!
//! 本示例展示了如何使用底层KCP API进行更精细的控制
//!
//! 与Stream API对比：
//! - Stream API: 自动处理update、socket读写，类似TCP
//! - 底层API: 需要手动管理所有细节，但更灵活

use kcp_ovo::{KcpConfig, core::kcp::Kcp};
use std::sync::{Arc, Mutex};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("KCP底层API示例");
    println!("==================");
    println!();

    // 示例1: 创建KCP实例
    example1_create_kcp()?;

    // 示例2: 配置KCP参数
    example2_configure_kcp()?;

    // 示例3: 发送和接收数据
    example3_send_recv()?;

    println!();
    println!("示例运行完成！");

    Ok(())
}

/// 示例1: 创建基本的KCP实例
fn example1_create_kcp() -> Result<(), Box<dyn std::error::Error>> {
    println!("示例1: 创建KCP实例");
    println!("-------------------");

    // 使用默认配置创建
    let kcp = Kcp::new(0x11223344, KcpConfig::default())?;
    println!("✓ 使用默认配置创建KCP实例");
    println!("  - conv: 0x{:08X}", kcp.conv);
    println!("  - MTU: {}", kcp.mtu);
    println!("  - MSS: {}", kcp.mss);
    println!();

    // 使用快速模式创建
    let kcp_fast = Kcp::new(0x11223344, KcpConfig::fast_mode())?;
    println!("✓ 使用快速模式创建KCP实例");
    println!("  - nodelay: {}", kcp_fast.nodelay);
    println!("  - interval: {}", kcp_fast.interval);
    println!("  - fastresend: {}", kcp_fast.fastresend);
    println!();

    Ok(())
}

/// 示例2: 配置KCP参数
fn example2_configure_kcp() -> Result<(), Box<dyn std::error::Error>> {
    println!("示例2: 自定义KCP配置");
    println!("---------------------");

    // 创建自定义配置
    let config = KcpConfig {
        mtu: 1400,
        interval: 50,
        nodelay: true,
        fastresend: 2,
        nocwnd: false,
        rcv_wnd: 512,
        ..Default::default()
    };

    let mut kcp = Kcp::new(0x11223344, config)?;
    println!("✓ 创建自定义配置的KCP实例");
    println!("  - MTU: {}", kcp.mtu);
    println!("  - 更新间隔: {} ms", kcp.interval);
    println!("  - 无延迟模式: {}", kcp.nodelay);
    println!("  - 快速重传: {}", kcp.fastresend);
    println!("  - 接收窗口: {}", kcp.rcv_wnd);
    println!();

    // 设置输出回调
    let sent_data = Arc::new(Mutex::new(Vec::new()));
    let sent_clone = sent_data.clone();

    kcp.set_output(move |data| {
        let sent_clone = sent_clone.clone();
        async move {
            println!("  [output] 发送 {} 字节", data.len());
            sent_clone.lock().unwrap().push(data.to_vec());
            Ok(data.len())
        }
    });

    println!("✓ 设置输出回调成功");
    println!();

    // 设置日志回调
    kcp.set_log(|msg, _kcp| {
        println!("  [log] {}", msg);
    });

    println!("✓ 设置日志回调成功");
    println!();

    Ok(())
}

/// 示例3: 发送和接收数据
fn example3_send_recv() -> Result<(), Box<dyn std::error::Error>> {
    println!("示例3: 发送和接收数据");
    println!("---------------------");

    // 创建KCP实例
    let mut kcp = Kcp::new(0x11223344, KcpConfig::default())?;

    // 设置输出回调（模拟发送）
    let sent_packets = Arc::new(Mutex::new(Vec::new()));
    let sent_clone = sent_packets.clone();

    kcp.set_output(move |data| {
        let sent_clone = sent_clone.clone();
        async move {
            println!("  [output] 发送 {} 字节", data.len());
            sent_clone.lock().unwrap().push(data.to_vec());
            Ok(data.len())
        }
    });

    println!("✓ KCP实例创建完成");

    // 发送数据
    let data1 = b"Hello, KCP!";
    let sent1 = kcp.send(data1)?;
    println!(
        "✓ 发送数据: \"{}\" ({} 字节)",
        String::from_utf8_lossy(data1),
        sent1
    );

    let data2 = b"Second message";
    let sent2 = kcp.send(data2)?;
    println!(
        "✓ 发送数据: \"{}\" ({} 字节)",
        String::from_utf8_lossy(data2),
        sent2
    );

    println!();

    // 模拟接收数据
    println!("模拟接收到的数据包...");
    println!("注意: 实际应用中需要通过UDP socket接收真实的数据包");

    // 尝试接收（会失败，因为队列为空）
    match kcp.recv() {
        Ok(n) => {
            println!(
                "✓ 接收到数据: \"{:?}\" ({:?} 字节)",
                String::from_utf8_lossy(&n),
                n.len()
            );
        }
        Err(e) => {
            println!("  接收失败: {} (队列为空，正常情况)", e);
        }
    }

    println!();

    // 查看队列状态
    println!("KCP状态:");
    println!("  - snd_queue: {} segments", kcp.nsnd_que);
    println!("  - waitsnd: {}", kcp.waitsnd());

    println!();

    Ok(())
}

#[allow(dead_code)]
/// 打印使用说明
fn print_usage() {
    println!("底层API vs Stream API:");
    println!("=======================");
    println!();
    println!("底层API (本示例):");
    println!("  优点:");
    println!("    - 完全控制KCP行为");
    println!("    - 可以精确管理update时机");
    println!("    - 适合集成到现有的事件循环");
    println!("  缺点:");
    println!("    - 需要手动管理socket");
    println!("    - 需要定期调用update()");
    println!("    - 需要处理更多的细节");
    println!();
    println!("Stream API (推荐大多数应用):");
    println!("  优点:");
    println!("    - 简单易用，类似TCP");
    println!("    - 自动处理update和socket读写");
    println!("    - 实现了Read/Write trait");
    println!("  缺点:");
    println!("    - 灵活性较低");
    println!("    - 额外的抽象层开销");
    println!();
    println!("选择建议:");
    println!("  - 新项目/简单应用: 使用Stream API");
    println!("  - 需要精细控制/现有集成: 使用底层API");
}
