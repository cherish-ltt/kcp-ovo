//! 固定种子损伤真实 UDP 报文；协议仍由高层后台任务驱动。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tokio::time::{Instant, interval};

use super::{Net, Rng};

#[derive(Clone, Copy, Default, Debug)]
pub struct RelayStats {
    pub max_client_packet: usize,
    pub max_server_packet: usize,
    pub dropped: usize,
    pub duplicated: usize,
    pub reordered: usize,
    pub blackout_dropped: usize,
    pub filtered: usize,
}

pub struct UdpRelay {
    pub addr: SocketAddr,
    pub stats: Arc<Mutex<RelayStats>>,
    task: Option<JoinHandle<()>>,
}

impl UdpRelay {
    pub async fn start(server: SocketAddr, net: Net, seed: u64) -> Self {
        Self::start_with_filter(server, net, seed, |_, _| false).await
    }

    /// `filter` 的第二个参数表示报文来自服务端，返回 true 时丢弃报文。
    pub async fn start_with_filter(
        server: SocketAddr,
        net: Net,
        seed: u64,
        mut filter: impl FnMut(&[u8], bool) -> bool + Send + 'static,
    ) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let stats = Arc::new(Mutex::new(RelayStats::default()));
        let observed = stats.clone();
        let task = tokio::spawn(async move {
            let mut rng = Rng::new(seed);
            let mut buf = vec![0; 65_535];
            let mut client = None;
            let mut pending = Vec::new();
            let mut timer = interval(Duration::from_millis(1));
            let started = Instant::now();
            let mut burst_bad = false;
            loop {
                tokio::select! {
                    _ = timer.tick() => {
                        let now = Instant::now();
                        let mut index = 0;
                        while index < pending.len() {
                            let (due, _, _) = &pending[index];
                            if *due > now {
                                index += 1;
                                continue;
                            }
                            let (_, packet, target): (_, Vec<u8>, _) = pending.remove(index);
                            socket.send_to(&packet, target).await.unwrap();
                        }
                    }
                    packet = socket.recv_from(&mut buf) => {
                        let (size, source) = packet.unwrap();
                        let target = if source == server {
                            client.expect("server replied before client sent")
                        } else {
                            client = Some(source);
                            server
                        };
                        let elapsed = started.elapsed().as_millis() as u64;
                        let blackout = net.in_blackout(elapsed);
                        if let Some(burst) = net.burst {
                            if burst_bad {
                                burst_bad = !rng.chance(burst.leave);
                            } else {
                                burst_bad = rng.chance(burst.enter);
                            }
                        }
                        let filtered = filter(&buf[..size], source == server);
                        let drop_packet = filtered || blackout || burst_bad || rng.chance(net.loss)
                            || net.path_mtu.is_some_and(|mtu| size > mtu);
                        let duplicate = !drop_packet && rng.chance(net.duplicate);
                        let reorder = !drop_packet && rng.chance(net.reorder);
                        {
                            let mut stats = observed.lock().unwrap();
                            if source == server {
                                stats.max_server_packet = stats.max_server_packet.max(size);
                            } else {
                                stats.max_client_packet = stats.max_client_packet.max(size);
                            }
                            stats.dropped += usize::from(drop_packet);
                            stats.blackout_dropped += usize::from(blackout);
                            stats.filtered += usize::from(filtered);
                            stats.duplicated += usize::from(duplicate);
                            stats.reordered += usize::from(reorder);
                        }
                        if drop_packet { continue; }
                        let jitter = if net.jitter == 0 { 0 } else { rng.next_u64() % (net.jitter + 1) };
                        let spike = net.spike.filter(|spike| rng.chance(spike.prob)).map_or(0, |spike| spike.extra);
                        let delay = net.delay + jitter + spike + if reorder { net.delay + 3 } else { 0 };
                        let due = Instant::now() + Duration::from_millis(delay);
                        if duplicate {
                            pending.push((due + Duration::from_millis(1), buf[..size].to_vec(), target));
                        }
                        pending.push((due, buf[..size].to_vec(), target));
                    }
                }
            }
        });
        Self { addr, stats, task: Some(task) }
    }

    pub async fn stop(mut self) {
        let task = self.task.take().unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled(), "UDP relay failed");
    }
}

impl Drop for UdpRelay {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
