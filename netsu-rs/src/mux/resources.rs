//! Best-effort process resource sampling (CPU%, resident memory) during a run,
//! summarized as mean/max. CPU is a percentage where 100% = one core.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::task::JoinHandle;

const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSummary {
    pub sample_count: u64,
    pub cpu_percent_mean: f64,
    pub cpu_percent_max: f64,
    pub resident_bytes_mean: u64,
    pub resident_bytes_max: u64,
}

/// Samples this process on a background blocking task until [`stop`].
pub struct ResourceSampler {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<ResourceSummary>,
}

impl ResourceSampler {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let handle = tokio::task::spawn_blocking(move || sample_loop(stop_c));
        Self { stop, handle }
    }

    pub async fn stop(self) -> ResourceSummary {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.await.unwrap_or(ResourceSummary {
            sample_count: 0,
            cpu_percent_mean: 0.0,
            cpu_percent_max: 0.0,
            resident_bytes_mean: 0,
            resident_bytes_max: 0,
        })
    }
}

fn sample_loop(stop: Arc<AtomicBool>) -> ResourceSummary {
    let pid = match sysinfo::get_current_pid() {
        Ok(p) => p,
        Err(_) => {
            return ResourceSummary {
                sample_count: 0,
                cpu_percent_mean: 0.0,
                cpu_percent_max: 0.0,
                resident_bytes_mean: 0,
                resident_bytes_max: 0,
            };
        }
    };
    let mut sys = System::new();
    let mut cpu_sum = 0.0f64;
    let mut cpu_max = 0.0f64;
    let mut rss_sum = 0u128;
    let mut rss_max = 0u64;
    let mut count = 0u64;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(SAMPLE_INTERVAL);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        if let Some(proc) = sys.process(pid) {
            let cpu = proc.cpu_usage() as f64;
            let rss = proc.memory();
            cpu_sum += cpu;
            cpu_max = cpu_max.max(cpu);
            rss_sum += rss as u128;
            rss_max = rss_max.max(rss);
            count += 1;
        }
    }

    let (cpu_mean, rss_mean) = if count > 0 {
        (cpu_sum / count as f64, (rss_sum / count as u128) as u64)
    } else {
        (0.0, 0)
    };
    ResourceSummary {
        sample_count: count,
        cpu_percent_mean: cpu_mean,
        cpu_percent_max: cpu_max,
        resident_bytes_mean: rss_mean,
        resident_bytes_max: rss_max,
    }
}

// Keep `Pid` import meaningful even if the type isn't named directly elsewhere.
const _: fn() -> Option<Pid> = || sysinfo::get_current_pid().ok();
