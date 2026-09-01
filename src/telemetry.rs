use std::{
    collections::HashMap,
    ffi::CString,
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{RwLock, broadcast};
use tracing::error;

use crate::db;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SystemSnapshot {
    pub sampled_at: i64,
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub architecture: String,
    pub uptime_seconds: f64,
    pub load_average: [f64; 3],
    pub cpu_percent: f64,
    pub per_core_percent: Vec<f64>,
    pub cpu_frequency_mhz: Option<u64>,
    pub memory: MemoryMetrics,
    pub swap: MemoryMetrics,
    pub disk: DiskMetrics,
    pub network: NetworkMetrics,
    pub temperatures: Vec<Temperature>,
    pub process_count: usize,
    pub carobaguard: SelfMetrics,
    pub telemetry_interval_seconds: u64,
    pub performance_mode: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MemoryMetrics {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub percent: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DiskMetrics {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub percent: f64,
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NetworkMetrics {
    pub received_bytes_per_sec: f64,
    pub transmitted_bytes_per_sec: f64,
    pub total_received_bytes: u64,
    pub total_transmitted_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Temperature {
    pub label: String,
    pub celsius: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SelfMetrics {
    pub cpu_percent: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
    pub network_bytes_per_sec: f64,
    pub db_writes_per_minute: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryProfile {
    Performance,
    Balanced,
    LowOverhead,
}

impl TelemetryProfile {
    pub fn interval(self) -> u64 {
        match self {
            Self::Performance => 5,
            Self::Balanced => 15,
            Self::LowOverhead => 30,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::LowOverhead => "low_overhead",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "performance" => Self::Performance,
            "low_overhead" => Self::LowOverhead,
            _ => Self::Balanced,
        }
    }
}

#[derive(Clone)]
pub struct TelemetryService {
    latest: Arc<RwLock<SystemSnapshot>>,
    events: broadcast::Sender<SystemSnapshot>,
    interval_seconds: Arc<AtomicU64>,
    performance_mode: Arc<AtomicBool>,
    db_writes: Arc<AtomicU64>,
    sse_bytes: Arc<AtomicU64>,
    started: Instant,
    db: SqlitePool,
}

impl TelemetryService {
    pub async fn start(db_pool: SqlitePool) -> anyhow::Result<Self> {
        let profile = db::setting(&db_pool, "telemetry_profile")
            .await?
            .map(|value| TelemetryProfile::parse(&value))
            .unwrap_or(TelemetryProfile::Balanced);
        let performance_mode = db::setting(&db_pool, "performance_mode")
            .await?
            .is_some_and(|value| value == "true");
        let interval = if performance_mode {
            60
        } else {
            profile.interval()
        };
        let raw = RawSnapshot::collect()?;
        let initial = build_snapshot(&raw, &raw, interval, performance_mode, 0, 0.0, 0.0)?;
        let (events, _) = broadcast::channel(32);
        let service = Self {
            latest: Arc::new(RwLock::new(initial)),
            events,
            interval_seconds: Arc::new(AtomicU64::new(interval)),
            performance_mode: Arc::new(AtomicBool::new(performance_mode)),
            db_writes: Arc::new(AtomicU64::new(0)),
            sse_bytes: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
            db: db_pool,
        };
        service.spawn_collector(raw);
        Ok(service)
    }

    pub async fn latest(&self) -> SystemSnapshot {
        self.latest.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SystemSnapshot> {
        self.events.subscribe()
    }

    pub fn record_sse_bytes(&self, bytes: u64) {
        self.sse_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub async fn configure(
        &self,
        profile: TelemetryProfile,
        performance_mode: bool,
    ) -> anyhow::Result<u64> {
        let interval = if performance_mode {
            60
        } else {
            profile.interval()
        };
        db::set_setting(&self.db, "telemetry_profile", profile.as_str()).await?;
        db::set_setting(
            &self.db,
            "performance_mode",
            if performance_mode { "true" } else { "false" },
        )
        .await?;
        self.db_writes.fetch_add(2, Ordering::Relaxed);
        self.interval_seconds.store(interval, Ordering::Relaxed);
        self.performance_mode
            .store(performance_mode, Ordering::Relaxed);
        let updated = {
            let mut latest = self.latest.write().await;
            latest.telemetry_interval_seconds = interval;
            latest.performance_mode = performance_mode;
            latest.clone()
        };
        let _ = self.events.send(updated);
        Ok(interval)
    }

    pub async fn history(&self, limit: i64) -> anyhow::Result<Vec<SystemSnapshot>> {
        let payloads: Vec<String> = sqlx::query_scalar(
            "SELECT payload_json FROM telemetry_samples ORDER BY sampled_at DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.db)
        .await?;
        let mut samples: Vec<SystemSnapshot> = payloads
            .into_iter()
            .filter_map(|value| serde_json::from_str(&value).ok())
            .collect();
        samples.reverse();
        Ok(samples)
    }

    fn spawn_collector(&self, mut previous: RawSnapshot) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut samples = 0_u64;
            loop {
                let interval = service.interval_seconds.load(Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(interval)).await;
                let current = match tokio::task::spawn_blocking(RawSnapshot::collect).await {
                    Ok(Ok(snapshot)) => snapshot,
                    Ok(Err(error)) => {
                        error!(%error, "telemetry collection failed");
                        continue;
                    }
                    Err(error) => {
                        error!(%error, "telemetry worker failed");
                        continue;
                    }
                };
                let elapsed = current
                    .collected_at
                    .duration_since(previous.collected_at)
                    .as_secs_f64()
                    .max(0.001);
                let writes = service.db_writes.load(Ordering::Relaxed);
                let writes_per_minute =
                    writes as f64 * 60.0 / service.started.elapsed().as_secs_f64().max(1.0);
                let sse_bytes = service.sse_bytes.swap(0, Ordering::Relaxed);
                let result = build_snapshot(
                    &previous,
                    &current,
                    interval,
                    service.performance_mode.load(Ordering::Relaxed),
                    database_size(&service.db).await.unwrap_or(0),
                    sse_bytes as f64 / elapsed,
                    writes_per_minute,
                );
                previous = current;
                let snapshot = match result {
                    Ok(value) => value,
                    Err(error) => {
                        error!(%error, "failed to calculate telemetry sample");
                        continue;
                    }
                };
                *service.latest.write().await = snapshot.clone();
                let subscribers = service.events.receiver_count() as u64;
                if subscribers > 0
                    && let Ok(payload) = serde_json::to_vec(&snapshot)
                {
                    service.record_sse_bytes(payload.len() as u64 * subscribers);
                }
                let _ = service.events.send(snapshot.clone());
                if let Err(error) = store_sample(&service.db, &snapshot, interval).await {
                    error!(%error, "failed to persist telemetry sample");
                } else {
                    service.db_writes.fetch_add(1, Ordering::Relaxed);
                }
                samples += 1;
                if samples.is_multiple_of(240) {
                    if let Err(error) = enforce_retention(&service.db).await {
                        error!(%error, "telemetry retention failed");
                    } else {
                        service.db_writes.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
    }
}

async fn store_sample(
    pool: &SqlitePool,
    sample: &SystemSnapshot,
    interval: u64,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(sample)?;
    sqlx::query(
        "INSERT INTO telemetry_samples(sampled_at, resolution_seconds, cpu_percent, \
         memory_used_bytes, memory_total_bytes, swap_used_bytes, disk_used_bytes, \
         disk_total_bytes, network_rx_bytes_per_sec, network_tx_bytes_per_sec, \
         disk_read_bytes_per_sec, disk_write_bytes_per_sec, carobaguard_cpu_percent, \
         carobaguard_memory_bytes, payload_json) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(sample.sampled_at)
    .bind(interval as i64)
    .bind(sample.cpu_percent)
    .bind(sample.memory.used_bytes as i64)
    .bind(sample.memory.total_bytes as i64)
    .bind(sample.swap.used_bytes as i64)
    .bind(sample.disk.used_bytes as i64)
    .bind(sample.disk.total_bytes as i64)
    .bind(sample.network.received_bytes_per_sec)
    .bind(sample.network.transmitted_bytes_per_sec)
    .bind(sample.disk.read_bytes_per_sec)
    .bind(sample.disk.write_bytes_per_sec)
    .bind(sample.carobaguard.cpu_percent)
    .bind(sample.carobaguard.memory_bytes as i64)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}

async fn enforce_retention(pool: &SqlitePool) -> anyhow::Result<()> {
    let days = db::setting(pool, "telemetry_retention_days")
        .await?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(7)
        .clamp(1, 365);
    sqlx::query("DELETE FROM telemetry_samples WHERE sampled_at < unixepoch() - (? * 86400)")
        .bind(days)
        .execute(pool)
        .await?;
    Ok(())
}

async fn database_size(pool: &SqlitePool) -> anyhow::Result<u64> {
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(pool)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(pool)
        .await?;
    Ok((page_count * page_size).max(0) as u64)
}

#[derive(Clone, Debug)]
struct RawSnapshot {
    collected_at: Instant,
    timestamp: i64,
    cpu: Vec<CpuCounter>,
    process_ticks: u64,
    memory: HashMap<String, u64>,
    network_rx: u64,
    network_tx: u64,
    disk_read_sectors: u64,
    disk_write_sectors: u64,
    uptime: f64,
    load: [f64; 3],
    process_count: usize,
    self_memory: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct CpuCounter {
    total: u64,
    idle: u64,
}

impl RawSnapshot {
    fn collect() -> anyhow::Result<Self> {
        let cpu = parse_cpu(&fs::read_to_string("/proc/stat")?)?;
        let memory = parse_meminfo(&fs::read_to_string("/proc/meminfo")?);
        let (network_rx, network_tx) = parse_network(&fs::read_to_string("/proc/net/dev")?)?;
        let (disk_read_sectors, disk_write_sectors) = read_block_stats()?;
        let uptime = fs::read_to_string("/proc/uptime")?
            .split_whitespace()
            .next()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        let load_values: Vec<f64> = fs::read_to_string("/proc/loadavg")?
            .split_whitespace()
            .take(3)
            .filter_map(|value| value.parse().ok())
            .collect();
        let load = [
            *load_values.first().unwrap_or(&0.0),
            *load_values.get(1).unwrap_or(&0.0),
            *load_values.get(2).unwrap_or(&0.0),
        ];
        let process_count = fs::read_dir("/proc")?
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
            })
            .count();
        Ok(Self {
            collected_at: Instant::now(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            cpu,
            process_ticks: process_ticks().unwrap_or(0),
            memory,
            network_rx,
            network_tx,
            disk_read_sectors,
            disk_write_sectors,
            uptime,
            load,
            process_count,
            self_memory: self_memory().unwrap_or(0),
        })
    }
}

fn build_snapshot(
    previous: &RawSnapshot,
    current: &RawSnapshot,
    interval: u64,
    performance_mode: bool,
    database_bytes: u64,
    sse_bytes_per_sec: f64,
    db_writes_per_minute: f64,
) -> anyhow::Result<SystemSnapshot> {
    let elapsed = current
        .collected_at
        .duration_since(previous.collected_at)
        .as_secs_f64()
        .max(0.001);
    let cpu_percentages: Vec<f64> = current
        .cpu
        .iter()
        .zip(&previous.cpu)
        .map(|(now, before)| counter_percent(*before, *now))
        .collect();
    let aggregate = *cpu_percentages.first().unwrap_or(&0.0);
    let per_core_percent = cpu_percentages.into_iter().skip(1).collect();
    let total_delta = current
        .cpu
        .first()
        .zip(previous.cpu.first())
        .map(|(now, before)| now.total.saturating_sub(before.total))
        .unwrap_or(0);
    let process_delta = current.process_ticks.saturating_sub(previous.process_ticks);
    let self_cpu = if total_delta == 0 {
        0.0
    } else {
        process_delta as f64 / total_delta as f64 * 100.0
    };

    let total_memory = mem(&current.memory, "MemTotal");
    let available_memory = mem(&current.memory, "MemAvailable");
    let used_memory = total_memory.saturating_sub(available_memory);
    let total_swap = mem(&current.memory, "SwapTotal");
    let free_swap = mem(&current.memory, "SwapFree");
    let used_swap = total_swap.saturating_sub(free_swap);
    let (disk_total, disk_used) = root_disk_usage()?;
    let executable_bytes = std::env::current_exe()
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    Ok(SystemSnapshot {
        sampled_at: current.timestamp,
        hostname: read_trimmed("/etc/hostname").unwrap_or_else(|| "unknown".into()),
        os: os_pretty_name(),
        kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into()),
        architecture: std::env::consts::ARCH.to_owned(),
        uptime_seconds: current.uptime,
        load_average: current.load,
        cpu_percent: aggregate,
        per_core_percent,
        cpu_frequency_mhz: read_cpu_frequency(),
        memory: usage(used_memory, total_memory),
        swap: usage(used_swap, total_swap),
        disk: DiskMetrics {
            used_bytes: disk_used,
            total_bytes: disk_total,
            percent: percent(disk_used, disk_total),
            read_bytes_per_sec: current
                .disk_read_sectors
                .saturating_sub(previous.disk_read_sectors) as f64
                * 512.0
                / elapsed,
            write_bytes_per_sec: current
                .disk_write_sectors
                .saturating_sub(previous.disk_write_sectors)
                as f64
                * 512.0
                / elapsed,
        },
        network: NetworkMetrics {
            received_bytes_per_sec: current.network_rx.saturating_sub(previous.network_rx) as f64
                / elapsed,
            transmitted_bytes_per_sec: current.network_tx.saturating_sub(previous.network_tx)
                as f64
                / elapsed,
            total_received_bytes: current.network_rx,
            total_transmitted_bytes: current.network_tx,
        },
        temperatures: read_temperatures(),
        process_count: current.process_count,
        carobaguard: SelfMetrics {
            cpu_percent: self_cpu,
            memory_bytes: current.self_memory,
            disk_bytes: executable_bytes + database_bytes,
            network_bytes_per_sec: sse_bytes_per_sec,
            db_writes_per_minute,
        },
        telemetry_interval_seconds: interval,
        performance_mode,
    })
}

fn parse_cpu(content: &str) -> anyhow::Result<Vec<CpuCounter>> {
    let mut values = Vec::new();
    for line in content.lines().take_while(|line| line.starts_with("cpu")) {
        let parts: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|value| value.parse().ok())
            .collect();
        if parts.len() < 4 {
            continue;
        }
        values.push(CpuCounter {
            total: parts.iter().sum(),
            idle: parts[3] + parts.get(4).copied().unwrap_or(0),
        });
    }
    anyhow::ensure!(!values.is_empty(), "no CPU counters found in /proc/stat");
    Ok(values)
}

fn parse_meminfo(content: &str) -> HashMap<String, u64> {
    content
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let kib = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((key.to_owned(), kib * 1024))
        })
        .collect()
}

fn parse_network(content: &str) -> anyhow::Result<(u64, u64)> {
    let mut received = 0_u64;
    let mut transmitted = 0_u64;
    for line in content.lines().skip(2) {
        let Some((interface, counters)) = line.split_once(':') else {
            continue;
        };
        if interface.trim() == "lo" {
            continue;
        }
        let values: Vec<u64> = counters
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if values.len() >= 16 {
            received = received.saturating_add(values[0]);
            transmitted = transmitted.saturating_add(values[8]);
        }
    }
    Ok((received, transmitted))
}

fn read_block_stats() -> anyhow::Result<(u64, u64)> {
    let mut read = 0_u64;
    let mut written = 0_u64;
    for entry in fs::read_dir("/sys/block")?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("loop") || name.starts_with("ram") {
            continue;
        }
        let content = match fs::read_to_string(entry.path().join("stat")) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let values: Vec<u64> = content
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if values.len() >= 7 {
            read = read.saturating_add(values[2]);
            written = written.saturating_add(values[6]);
        }
    }
    Ok((read, written))
}

fn counter_percent(previous: CpuCounter, current: CpuCounter) -> f64 {
    let total = current.total.saturating_sub(previous.total);
    let idle = current.idle.saturating_sub(previous.idle);
    if total == 0 {
        0.0
    } else {
        (total.saturating_sub(idle)) as f64 / total as f64 * 100.0
    }
}

fn root_disk_usage() -> anyhow::Result<(u64, u64)> {
    let path = CString::new("/").expect("static path contains no NUL");
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid NUL-terminated string and `stats` points to writable memory.
    let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("statvfs(/) failed");
    }
    // SAFETY: successful statvfs initialized the complete output structure.
    let stats = unsafe { stats.assume_init() };
    let block_size = stats.f_frsize;
    let total = stats.f_blocks * block_size;
    let available = stats.f_bavail * block_size;
    Ok((total, total.saturating_sub(available)))
}

fn process_ticks() -> anyhow::Result<u64> {
    let stat = fs::read_to_string("/proc/self/stat")?;
    let after_name = stat
        .rsplit_once(')')
        .context("invalid /proc/self/stat format")?
        .1;
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    let user: u64 = fields.get(11).context("missing process utime")?.parse()?;
    let system: u64 = fields.get(12).context("missing process stime")?.parse()?;
    Ok(user + system)
}

fn self_memory() -> anyhow::Result<u64> {
    let content = fs::read_to_string("/proc/self/status")?;
    let kib = content
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(kib * 1024)
}

fn read_temperatures() -> Vec<Temperature> {
    let mut temperatures = Vec::new();
    let Ok(zones) = fs::read_dir("/sys/class/thermal") else {
        return temperatures;
    };
    for zone in zones.flatten().filter(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .starts_with("thermal_zone")
    }) {
        let path = zone.path();
        let Some(raw) = read_trimmed(path.join("temp")) else {
            continue;
        };
        let Ok(mut celsius) = raw.parse::<f64>() else {
            continue;
        };
        if celsius.abs() > 1000.0 {
            celsius /= 1000.0;
        }
        if !(-50.0..=250.0).contains(&celsius) {
            continue;
        }
        temperatures.push(Temperature {
            label: read_trimmed(path.join("type"))
                .unwrap_or_else(|| zone.file_name().to_string_lossy().into_owned()),
            celsius,
        });
    }
    temperatures
}

fn read_cpu_frequency() -> Option<u64> {
    read_trimmed("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")?
        .parse::<u64>()
        .ok()
        .map(|khz| khz / 1000)
}

fn os_pretty_name() -> String {
    let content = fs::read_to_string("/etc/os-release").unwrap_or_default();
    content
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("PRETTY_NAME=")?;
            Some(value.trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| "Linux".to_owned())
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn mem(values: &HashMap<String, u64>, key: &str) -> u64 {
    values.get(key).copied().unwrap_or(0)
}

fn usage(used: u64, total: u64) -> MemoryMetrics {
    MemoryMetrics {
        used_bytes: used,
        total_bytes: total,
        percent: percent(used, total),
    }
}

fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64 * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_cpu_and_calculates_busy_time() {
        let before = parse_cpu("cpu  10 0 5 85 0 0 0 0\ncpu0 10 0 5 85 0 0 0 0\n").unwrap();
        let after = parse_cpu("cpu  30 0 10 160 0 0 0 0\ncpu0 30 0 10 160 0 0 0 0\n").unwrap();
        assert!((counter_percent(before[0], after[0]) - 25.0).abs() < 0.01);
    }

    #[test]
    fn ignores_loopback_network_traffic() {
        let sample = "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n lo: 100 0 0 0 0 0 0 0 200 0 0 0 0 0 0 0\n eth0: 500 0 0 0 0 0 0 0 900 0 0 0 0 0 0 0\n";
        assert_eq!(parse_network(sample).unwrap(), (500, 900));
    }

    #[test]
    fn meminfo_values_are_converted_from_kibibytes() {
        let values = parse_meminfo("MemTotal: 1024 kB\nMemAvailable: 512 kB\n");
        assert_eq!(values["MemTotal"], 1024 * 1024);
    }
}
