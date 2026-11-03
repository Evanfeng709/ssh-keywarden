//! Linux 状态监控：一次 exec 读取 /proc、df、ps，远端双采样算 CPU 与网络速率。
//!
//! 只跑只读命令，不写远端文件、不驻留进程；非 Linux 或受限账号会返回明确错误。

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::session::{SshClient, SshError};

/// 两次采样之间的窗口，用于把累计计数换算成瞬时占用/速率。
const SAMPLE_WINDOW: f64 = 0.6;
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(12);

const SAMPLE_COMMAND: &str = r#"export LC_ALL=C
echo @@OS
(. /etc/os-release 2>/dev/null && printf '%s\n' "$PRETTY_NAME") || uname -sr
echo @@UPTIME
cat /proc/uptime 2>/dev/null
echo @@LOAD
cat /proc/loadavg 2>/dev/null
echo @@STAT1
cat /proc/stat 2>/dev/null
echo @@MEM
cat /proc/meminfo 2>/dev/null
echo @@NET1
cat /proc/net/dev 2>/dev/null
sleep 0.6
echo @@STAT2
cat /proc/stat 2>/dev/null
echo @@NET2
cat /proc/net/dev 2>/dev/null
echo @@DF
df -P -B1 2>/dev/null
echo @@PS
ps -eo pid=,comm=,pcpu=,pmem= --sort=-pcpu 2>/dev/null | head -n 7
echo @@END
"#;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorSnapshot {
    pub os: String,
    pub uptime_secs: f64,
    pub cores: usize,
    pub cpu: CpuStats,
    pub mem: MemStats,
    pub load: LoadStats,
    pub net: NetStats,
    pub disks: Vec<DiskUsage>,
    pub processes: Vec<ProcUsage>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CpuStats {
    pub total: f64,
    pub user: f64,
    pub system: f64,
    pub iowait: f64,
    pub per_core: Vec<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemStats {
    pub total: f64,
    pub used: f64,
    pub available: f64,
    pub cached: f64,
    pub swap_total: f64,
    pub swap_used: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadStats {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
    pub runnable: u64,
    pub processes: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetStats {
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
    pub rx_total: f64,
    pub tx_total: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub fs: String,
    pub mount: String,
    pub total: f64,
    pub used: f64,
    pub available: f64,
    pub percent: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcUsage {
    pub pid: u32,
    pub comm: String,
    pub cpu: f64,
    pub mem: f64,
    pub mem_bytes: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuTimes {
    fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }
}

pub async fn collect(client: &SshClient) -> Result<MonitorSnapshot, SshError> {
    let res = client
        .exec_with_timeout(SAMPLE_COMMAND, None, SAMPLE_TIMEOUT)
        .await?;
    parse_snapshot(&res.stdout)
}

/// 解析采样输出。拆出来单独测，避免每次都要起 SSH。
pub fn parse_snapshot(text: &str) -> Result<MonitorSnapshot, SshError> {
    let sections = split_sections(text);
    let stat1 = sections.get("STAT1").map(String::as_str).unwrap_or("");
    let stat2 = sections.get("STAT2").map(String::as_str).unwrap_or("");
    let mem_raw = sections.get("MEM").map(String::as_str).unwrap_or("");
    if stat1.trim().is_empty() || mem_raw.trim().is_empty() {
        return Err(SshError::msg(
            "服务器没有返回监控数据：状态监控只支持 Linux，且需要能执行只读命令的账号",
        ));
    }

    let (cpu, cores) = parse_cpu(stat1, stat2);
    let mem_values = parse_meminfo(mem_raw);
    let mem = MemStats {
        total: mem_values.0,
        used: (mem_values.0 - mem_values.1).max(0.0),
        available: mem_values.1,
        cached: mem_values.2,
        swap_total: mem_values.3,
        swap_used: (mem_values.3 - mem_values.4).max(0.0),
    };
    let load = parse_load(&text_field(&sections, "LOAD"));
    let net = parse_net(
        &text_field(&sections, "NET1"),
        &text_field(&sections, "NET2"),
    );
    let disks = parse_disks(&text_field(&sections, "DF"));
    let processes = parse_processes(
        &text_field(&sections, "PS"),
        if mem.total > 0.0 { mem.total } else { 0.0 },
    );
    let mut os = text_field(&sections, "OS");
    os.truncate(120);
    let uptime_secs = text_field(&sections, "UPTIME")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);

    Ok(MonitorSnapshot {
        os,
        uptime_secs,
        cores,
        cpu,
        mem,
        load,
        net,
        disks,
        processes,
    })
}

fn text_field(sections: &std::collections::HashMap<String, String>, name: &str) -> String {
    sections
        .get(name)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn split_sections(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut cur = String::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            cur = name.trim().to_string();
            out.entry(cur.clone()).or_insert_with(String::new);
        } else if !cur.is_empty() {
            let buf = out.entry(cur.clone()).or_insert_with(String::new);
            buf.push_str(line);
            buf.push('\n');
        }
    }
    out
}

fn parse_cpu(stat1: &str, stat2: &str) -> (CpuStats, usize) {
    let first = parse_cpu_lines(stat1);
    let second = parse_cpu_lines(stat2);
    let cores = second
        .iter()
        .filter(|(name, _)| name != "cpu")
        .count()
        .max(first.iter().filter(|(name, _)| name != "cpu").count());

    let total = first
        .iter()
        .find(|(name, _)| name == "cpu")
        .and_then(|(_, a)| {
            second
                .iter()
                .find(|(name, _)| name == "cpu")
                .map(|(_, b)| share(a, b))
        });
    let total_share = total.unwrap_or_default();

    let per_core = second
        .iter()
        .filter(|(name, _)| name != "cpu")
        .map(|(name, b)| {
            first
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, a)| share(a, b).total)
                .unwrap_or(0.0)
        })
        .collect();

    (
        CpuStats {
            total: total_share.total,
            user: total_share.user,
            system: total_share.system,
            iowait: total_share.iowait,
            per_core,
        },
        cores,
    )
}

fn parse_cpu_lines(raw: &str) -> Vec<(String, CpuTimes)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut it = line.split_whitespace();
        let Some(name) = it.next() else { continue };
        if !name.starts_with("cpu") {
            continue;
        }
        let vals: Vec<u64> = it.take(8).filter_map(|v| v.parse().ok()).collect();
        if vals.len() < 4 {
            continue;
        }
        let get = |i: usize| vals.get(i).copied().unwrap_or(0);
        out.push((
            name.to_string(),
            CpuTimes {
                user: get(0),
                nice: get(1),
                system: get(2),
                idle: get(3),
                iowait: get(4),
                irq: get(5),
                softirq: get(6),
                steal: get(7),
            },
        ));
    }
    out
}

#[derive(Debug, Clone, Copy, Default)]
struct CpuShare {
    total: f64,
    user: f64,
    system: f64,
    iowait: f64,
}

fn share(a: &CpuTimes, b: &CpuTimes) -> CpuShare {
    let total = b.total().saturating_sub(a.total()) as f64;
    if total <= 0.0 {
        return CpuShare::default();
    }
    let user = (b.user + b.nice).saturating_sub(a.user + a.nice) as f64;
    let system = (b.system + b.irq + b.softirq).saturating_sub(a.system + a.irq + a.softirq) as f64;
    let iowait = b.iowait.saturating_sub(a.iowait) as f64;
    let idle = b.idle.saturating_sub(a.idle) as f64;
    CpuShare {
        total: clamp_pct((total - idle) / total * 100.0),
        user: clamp_pct(user / total * 100.0),
        system: clamp_pct(system / total * 100.0),
        iowait: clamp_pct(iowait / total * 100.0),
    }
}

fn clamp_pct(v: f64) -> f64 {
    (v * 10.0).round().clamp(0.0, 1000.0) / 10.0
}

fn parse_meminfo(raw: &str) -> (f64, f64, f64, f64, f64) {
    let mut total = 0.0;
    let mut available = 0.0;
    let mut free = 0.0;
    let mut buffers = 0.0;
    let mut cached = 0.0;
    let mut swap_total = 0.0;
    let mut swap_free = 0.0;
    for line in raw.lines() {
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        let value = it.next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) * 1024.0;
        match key {
            "MemTotal:" => total = value,
            "MemAvailable:" => available = value,
            "MemFree:" => free = value,
            "Buffers:" => buffers = value,
            "Cached:" => cached = value,
            "SwapTotal:" => swap_total = value,
            "SwapFree:" => swap_free = value,
            _ => {}
        }
    }
    if available <= 0.0 {
        available = free + buffers + cached;
    }
    (total, available, buffers + cached, swap_total, swap_free)
}

fn parse_load(raw: &str) -> LoadStats {
    let mut it = raw.split_whitespace();
    let one = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let five = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let fifteen = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let (runnable, processes) = it
        .next()
        .and_then(|v| v.split_once('/'))
        .map(|(a, b)| (a.parse::<u64>().unwrap_or(0), b.parse::<u64>().unwrap_or(0)))
        .unwrap_or((0, 0));
    LoadStats {
        one,
        five,
        fifteen,
        runnable,
        processes,
    }
}

fn parse_net(first: &str, second: &str) -> NetStats {
    let (rx1, tx1) = net_counters(first);
    let (rx2, tx2) = net_counters(second);
    NetStats {
        rx_bytes_per_sec: ((rx2 - rx1).max(0.0) / SAMPLE_WINDOW * 10.0).round() / 10.0,
        tx_bytes_per_sec: ((tx2 - tx1).max(0.0) / SAMPLE_WINDOW * 10.0).round() / 10.0,
        rx_total: rx2,
        tx_total: tx2,
    }
}

fn net_counters(raw: &str) -> (f64, f64) {
    let mut rx = 0.0;
    let mut tx = 0.0;
    for line in raw.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name == "lo" {
            continue;
        }
        let nums: Vec<f64> = rest
            .split_whitespace()
            .filter_map(|v| v.parse::<f64>().ok())
            .collect();
        if nums.len() >= 9 {
            rx += nums[0];
            tx += nums[8];
        }
    }
    (rx, tx)
}

fn parse_disks(raw: &str) -> Vec<DiskUsage> {
    let mut out = Vec::new();
    for line in raw.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }
        let total = cols[1].parse::<f64>().unwrap_or(0.0);
        let used = cols[2].parse::<f64>().unwrap_or(0.0);
        let available = cols[3].parse::<f64>().unwrap_or(0.0);
        if total <= 0.0 {
            continue;
        }
        let fs = cols[0];
        if matches!(fs, "tmpfs" | "devtmpfs" | "udev" | "shm" | "ramfs" | "none") || cols[2] == "0"
        {
            continue;
        }
        let percent = cols[4]
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap_or_else(|_| used / total * 100.0);
        let mount = cols[5..].join(" ");
        out.push(DiskUsage {
            fs: fs.to_string(),
            mount,
            total,
            used,
            available,
            percent: clamp_pct(percent),
        });
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn parse_processes(raw: &str, mem_total: f64) -> Vec<ProcUsage> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let Some(pid) = cols[0].parse::<u32>().ok() else {
            continue;
        };
        let mem = cols[cols.len() - 1].parse::<f64>().unwrap_or(0.0);
        let cpu = cols[cols.len() - 2].parse::<f64>().unwrap_or(0.0);
        let comm = cols[1..cols.len() - 2].join(" ");
        out.push(ProcUsage {
            pid,
            comm,
            cpu,
            mem,
            mem_bytes: mem_total * mem / 100.0,
        });
        if out.len() >= 6 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"@@OS
Ubuntu 24.04.1 LTS
@@UPTIME
3690456.78 1234567.89
@@STAT1
cpu  100 0 50 850 0 0 0 0 0 0
cpu0 50 0 25 425 0 0 0 0 0 0
cpu1 50 0 25 425 0 0 0 0 0 0
intr 0
@@LOAD
0.42 0.35 0.28 1/187 12345
@@MEM
MemTotal:       16000000 kB
MemFree:         2000000 kB
MemAvailable:    6000000 kB
Buffers:          500000 kB
Cached:          2500000 kB
SwapTotal:       2000000 kB
SwapFree:        1600000 kB
@@NET1
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 999999 0 0 0 0 0 0 0 999999 0 0 0 0 0 0 0
  eth0: 1000 0 0 0 0 0 0 0 500 0 0 0 0 0 0 0
@@STAT2
cpu  160 0 70 970 0 0 0 0 0 0
cpu0 80 0 35 485 0 0 0 0 0 0
cpu1 80 0 35 485 0 0 0 0 0 0
intr 0
@@NET2
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1099999 0 0 0 0 0 0 0 1099999 0 0 0 0 0 0 0
  eth0: 3000 0 0 0 0 0 0 0 1100 0 0 0 0 0 0 0
@@DF
Filesystem     1-blocks      Used Available Capacity Mounted on
/dev/sda1     105648128  49545216  50712576      50% /
tmpfs           8000000         0   8000000       0% /dev/shm
/dev/sda2     123456789 123456789 987654321      13% /data
@@PS
 1841 nginx       12.4  7.5
  927 postgres     8.1 15.0
@@END
"#;

    #[test]
    fn parses_fixture_snapshot() {
        let snap = parse_snapshot(FIXTURE).unwrap();
        assert_eq!(snap.os, "Ubuntu 24.04.1 LTS");
        assert_eq!(snap.cores, 2);
        assert!((snap.uptime_secs - 3690456.78).abs() < 0.01);

        // 总增量 200：user 60、system 20、idle 120 → 占用 40%
        assert!((snap.cpu.total - 40.0).abs() < 0.01);
        assert!((snap.cpu.user - 30.0).abs() < 0.01);
        assert!((snap.cpu.system - 10.0).abs() < 0.01);
        assert_eq!(snap.cpu.per_core.len(), 2);
        assert!((snap.cpu.per_core[0] - 40.0).abs() < 0.01);

        assert!((snap.mem.total - 16_000_000.0 * 1024.0).abs() < 1.0);
        assert!((snap.mem.used - 10_000_000.0 * 1024.0).abs() < 1.0);
        assert!((snap.mem.swap_used - 400_000.0 * 1024.0).abs() < 1.0);

        assert!((snap.load.one - 0.42).abs() < 0.001);
        assert!((snap.load.five - 0.35).abs() < 0.001);
        assert_eq!(snap.load.runnable, 1);
        assert_eq!(snap.load.processes, 187);

        assert!((snap.net.rx_bytes_per_sec - 3333.3).abs() < 1.0);
        assert!((snap.net.tx_bytes_per_sec - 1000.0).abs() < 1.0);
        assert!((snap.net.rx_total - 3000.0).abs() < 0.01);

        assert_eq!(snap.disks.len(), 2, "tmpfs 不应出现在磁盘列表");
        assert_eq!(snap.disks[0].mount, "/");
        assert!((snap.disks[0].percent - 50.0).abs() < 0.01);
        assert_eq!(snap.disks[1].mount, "/data");

        assert_eq!(snap.processes.len(), 2);
        assert_eq!(snap.processes[0].comm, "nginx");
        assert!((snap.processes[0].cpu - 12.4).abs() < 0.01);
        assert!(snap.processes[0].mem_bytes > 0.0);
    }

    #[test]
    fn rejects_non_linux_output() {
        let err = parse_snapshot("bash: /proc/stat: No such file or directory").unwrap_err();
        assert!(err.to_string().contains("只支持 Linux"));
    }
}
