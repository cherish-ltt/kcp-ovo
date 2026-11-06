use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::core::kcp::Kcp;
use crate::{KcpError, KcpResult};

/// 同一连接的一次完整协议采样，不代表对端在线或应用已经读取数据。
///
/// 后台每轮驱动后尝试更新缓存；发送背压、UDP 输出等待或查询竞争时采样可能陈旧。
/// 使用 `sampled_at.elapsed()` 判断时效，查询本身不会驱动协议或消费消息。
#[derive(Debug, Clone, Copy)]
pub struct KcpDiagnostics {
    /// 平滑 RTT，单位毫秒；尚无有效样本时为 `None`，有效的零毫秒样本为 `Some(0)`。
    pub smoothed_rtt_ms: Option<u64>,
    /// 本地重传超时估计，单位毫秒，不是实测网络延迟。
    pub retransmission_timeout_ms: u64,
    /// 对端最近通告的剩余窗口，单位段；未见对端时为 `None`，已知零窗口为 `Some(0)`。
    pub remote_window_segments: Option<u16>,
    /// 本地拥塞窗口，单位段；禁用拥塞控制时不限制发送。
    pub congestion_window_segments: u16,
    /// KCP 内部尚未发送的段数，不包含高层发送通道中尚未被消费的数据。
    pub queued_send_segments: usize,
    /// KCP 已发送但尚未累计确认的段数；与待发段数之和等于底层 `waitsnd()`。
    pub in_flight_segments: usize,
    /// 本次完整采样的单调时钟时刻，可能早于查询时刻。
    pub sampled_at: Instant,
}

impl KcpDiagnostics {
    fn sample(kcp: &Kcp) -> Self {
        Self {
            smoothed_rtt_ms: kcp.rtt_sampled.then_some(kcp.rx_srtt as u64),
            retransmission_timeout_ms: kcp.rx_rto,
            remote_window_segments: kcp.peer_seen.then_some(kcp.rmt_wnd),
            congestion_window_segments: kcp.cwnd,
            queued_send_segments: kcp.nsnd_que,
            in_flight_segments: kcp.nsnd_buf,
            sampled_at: Instant::now(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Diagnostics(Arc<Mutex<KcpDiagnostics>>);

impl Diagnostics {
    pub(super) fn new(kcp: &Kcp) -> Self {
        Self(Arc::new(Mutex::new(KcpDiagnostics::sample(kcp))))
    }

    pub(super) fn update(&self, kcp: &Kcp) {
        if let Ok(mut snapshot) = self.0.try_lock() {
            *snapshot = KcpDiagnostics::sample(kcp);
        }
    }

    pub(super) fn get(&self) -> KcpResult<KcpDiagnostics> {
        self.0
            .try_lock()
            .map(|snapshot| *snapshot)
            .map_err(|error| KcpError::IoError(format!("diagnostics unavailable: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KcpConfig;
    use crate::helper::{KcpCmd, KcpPacketHeader};

    #[test]
    fn query_and_sampling_never_wait_for_another_reader() {
        let kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        let cache = Diagnostics::new(&kcp);
        let held = cache.0.lock().unwrap();
        assert!(cache.get().is_err());
        cache.update(&kcp);
        assert_eq!(held.queued_send_segments, 0);
    }

    #[tokio::test]
    async fn valid_zero_rtt_is_known_but_retransmitted_rtt_stays_unknown() {
        for retransmit in [false, true] {
            let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
            kcp.set_output(|data| async move { Ok(data.len()) });
            kcp.send(b"sample").unwrap();
            kcp.update(1000).await.unwrap();
            if retransmit {
                kcp.update(2000).await.unwrap();
            }
            let header = KcpPacketHeader::new(1, KcpCmd::Ack, 0, 128, 1000, 0, 1, 0);
            kcp.input(&header.to_bytes().unwrap()).unwrap();
            assert_eq!(KcpDiagnostics::sample(&kcp).smoothed_rtt_ms, (!retransmit).then_some(0));
        }
    }

    #[tokio::test]
    async fn blocked_output_preserves_sample_age_without_blocking_queries() {
        let mut kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        kcp.set_output(|_| std::future::pending());
        kcp.send(b"pending output").unwrap();
        let cache = Diagnostics::new(&kcp);
        let sampled_at = cache.get().unwrap().sampled_at;
        let update = kcp.update(1000);
        tokio::pin!(update);
        tokio::select! {
            biased;
            result = &mut update => panic!("output did not block: {result:?}"),
            () = std::future::ready(()) => {},
        }
        for _ in 0..1000 {
            let snapshot = cache.get().unwrap();
            assert_eq!(snapshot.sampled_at, sampled_at);
            assert_eq!(snapshot.queued_send_segments, 1);
            assert_eq!(snapshot.in_flight_segments, 0);
        }
    }

    #[test]
    #[ignore = "manual sampling-cost measurement; run with --release --ignored --nocapture"]
    fn sampling_cost() {
        let kcp = Kcp::new(1, KcpConfig::fast_mode()).unwrap();
        let cache = Diagnostics::new(&kcp);
        let iterations = 1_000_000;
        let started = Instant::now();
        for _ in 0..iterations {
            cache.update(std::hint::black_box(&kcp));
        }
        let update_ns = started.elapsed().as_nanos() / iterations;
        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(cache.get().unwrap());
        }
        let query_ns = started.elapsed().as_nanos() / iterations;
        println!(
            "diagnostics: {update_ns} ns/sample, {query_ns} ns/query ({iterations} iterations)"
        );
    }
}
