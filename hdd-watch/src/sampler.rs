use std::collections::HashMap;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use crate::logfmt::{self, LogSample, ProcKbps};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemInfo {
    pub total_kb: u64,
    pub available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTimes {
    pub total: u64,
    pub iowait: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcStat {
    pub pid: u32,
    pub ppid: u32,
    pub pgrp: u32,
    pub name: String,
    pub state: char,
    pub utime: u64,
    pub stime: u64,
    pub majflt: u64,
    pub delayacct: u64,
    pub starttime: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcIo {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub rchar: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IoRates {
    pub name: String,
    pub read_kbps: f64,
    pub write_kbps: f64,
}

impl IoRates {
    pub fn total_kbps(&self) -> f64 {
        self.read_kbps + self.write_kbps
    }
}

pub fn parse_psi_avg10(content: &str) -> Option<f64> {
    let line = content
        .lines()
        .find(|line| line.trim_start().starts_with("some "))
        .or_else(|| content.lines().next())?;
    line.split_whitespace()
        .find_map(|token| token.strip_prefix("avg10="))
        .and_then(|value| value.parse::<f64>().ok())
}

pub fn read_psi(resource: &str) -> Option<f64> {
    let path = format!("/proc/pressure/{resource}");
    fs::read_to_string(path)
        .ok()
        .and_then(|content| parse_psi_avg10(&content))
}

pub fn parse_meminfo(content: &str) -> MemInfo {
    let mut info = MemInfo::default();
    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = rest.split_whitespace().next().and_then(|v| v.parse().ok()) else {
            continue;
        };
        match key {
            "MemTotal" => info.total_kb = value,
            "MemAvailable" => info.available_kb = value,
            "SwapTotal" => info.swap_total_kb = value,
            "SwapFree" => info.swap_free_kb = value,
            _ => {}
        }
    }
    info
}

pub fn read_meminfo() -> MemInfo {
    fs::read_to_string("/proc/meminfo")
        .map(|content| parse_meminfo(&content))
        .unwrap_or_default()
}

pub fn parse_cpu_line(content: &str) -> Option<CpuTimes> {
    let line = content.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 8 {
        return None;
    }
    let total = values
        .iter()
        .take(8)
        .fold(0u64, |acc, v| acc.saturating_add(*v));
    Some(CpuTimes {
        total,
        iowait: values[4],
    })
}

#[derive(Clone, Debug, Default)]
pub struct CpuSampler {
    previous: Option<CpuTimes>,
}

impl CpuSampler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn prime(&mut self) {
        self.previous = fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|content| parse_cpu_line(&content));
    }

    pub fn sample(&mut self) -> f64 {
        let current = fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|content| parse_cpu_line(&content));
        let Some(current) = current else {
            return 0.0;
        };
        let previous = self.previous.replace(current);
        let Some(previous) = previous else {
            return 0.0;
        };
        let total_delta = current.total.saturating_sub(previous.total);
        let iowait_delta = current.iowait.saturating_sub(previous.iowait);
        if total_delta == 0 {
            0.0
        } else {
            (iowait_delta as f64 / total_delta as f64) * 100.0
        }
    }
}

pub fn parse_stat_line(line: &str) -> Option<ProcStat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close <= open {
        return None;
    }
    let pid = line.get(..open)?.trim().parse().ok()?;
    let name = line.get(open.saturating_add(1)..close)?.to_string();
    let rest = line.get(close.saturating_add(1)..)?.trim();
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() < 20 {
        return None;
    }
    let at = |idx: usize| -> Option<u64> { tokens.get(idx).and_then(|v| v.parse().ok()) };
    let at32 = |idx: usize| -> Option<u32> { tokens.get(idx).and_then(|v| v.parse().ok()) };
    let state = tokens.first().and_then(|s| s.chars().next()).unwrap_or('?');
    Some(ProcStat {
        pid,
        ppid: at32(1)?,
        pgrp: at32(2)?,
        name,
        state,
        utime: at(11).unwrap_or(0),
        stime: at(12).unwrap_or(0),
        majflt: at(9).unwrap_or(0),
        delayacct: at(39).unwrap_or(0),
        starttime: at(19).unwrap_or(0),
    })
}

pub fn read_all_procs() -> Vec<ProcStat> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let path = format!("/proc/{name}/stat");
        let Ok(line) = fs::read_to_string(path) else {
            continue;
        };
        if let Some(stat) = parse_stat_line(&line) {
            out.push(stat);
        }
    }
    out
}

pub fn read_proc_io(pid: u32) -> Option<ProcIo> {
    let content = fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    parse_proc_io(&content)
}

pub fn parse_proc_io(content: &str) -> Option<ProcIo> {
    let mut io = ProcIo::default();
    let mut seen = false;
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().parse::<u64>().ok()?;
        match key {
            "read_bytes" => {
                io.read_bytes = value;
                seen = true;
            }
            "write_bytes" => io.write_bytes = value,
            "rchar" => io.rchar = value,
            _ => {}
        }
    }
    if seen {
        Some(io)
    } else {
        None
    }
}

pub fn group_members(procs: &[ProcStat], pgid: u32) -> Vec<&ProcStat> {
    procs.iter().filter(|p| p.pgrp == pgid).collect()
}

#[derive(Clone, Debug, Default)]
pub struct IoAggregator {
    previous: HashMap<u32, (u64, u64)>,
}

impl IoAggregator {
    pub fn prime(&mut self, procs: &[ProcStat]) {
        self.previous.clear();
        for proc in procs {
            if let Some(io) = read_proc_io(proc.pid) {
                self.previous
                    .insert(proc.pid, (io.read_bytes, io.write_bytes));
            }
        }
    }

    pub fn sample(&mut self, procs: &[ProcStat], elapsed: Duration) -> Vec<IoRates> {
        let secs = elapsed.as_secs_f64().max(0.001);
        let mut next = HashMap::new();
        let mut by_name: HashMap<String, (f64, f64)> = HashMap::new();
        for proc in procs {
            let Some(io) = read_proc_io(proc.pid) else {
                continue;
            };
            let (prev_read, prev_write) = self
                .previous
                .get(&proc.pid)
                .copied()
                .unwrap_or((io.read_bytes, io.write_bytes));
            let read_delta = io.read_bytes.saturating_sub(prev_read);
            let write_delta = io.write_bytes.saturating_sub(prev_write);
            next.insert(proc.pid, (io.read_bytes, io.write_bytes));
            let name = logfmt::sanitize_name(&proc.name);
            let entry = by_name.entry(name).or_insert((0.0, 0.0));
            entry.0 += read_delta as f64 / 1024.0 / secs;
            entry.1 += write_delta as f64 / 1024.0 / secs;
        }
        self.previous = next;
        let mut rates: Vec<IoRates> = by_name
            .into_iter()
            .map(|(name, (read_kbps, write_kbps))| IoRates {
                name,
                read_kbps,
                write_kbps,
            })
            .filter(|rate| rate.total_kbps() >= 0.05)
            .collect();
        rates.sort_by(|a, b| b.total_kbps().total_cmp(&a.total_kbps()));
        rates
    }
}

#[derive(Clone, Debug)]
pub struct LiveSample {
    pub log: LogSample,
    pub rates: Vec<IoRates>,
}

pub fn sample_live(cpu: &mut CpuSampler, io: &mut IoAggregator, elapsed: Duration) -> LiveSample {
    let mem = read_meminfo();
    let procs = read_all_procs();
    let rates = io.sample(&procs, elapsed);
    let top: Vec<ProcKbps> = rates
        .iter()
        .take(3)
        .map(|rate| ProcKbps {
            name: rate.name.clone(),
            kbps: rate.total_kbps(),
        })
        .collect();
    let log = LogSample {
        ts: logfmt::now_epoch(),
        io_psi: read_psi("io").unwrap_or(0.0),
        mem_psi: read_psi("memory").unwrap_or(0.0),
        iowait_pct: cpu.sample(),
        mem_avail_mb: mem.available_kb / 1024,
        swap_used_mb: mem.swap_total_kb.saturating_sub(mem.swap_free_kb) / 1024,
        procs: top,
    };
    LiveSample { log, rates }
}

pub fn sample_once(cpu: &mut CpuSampler, io: &mut IoAggregator, elapsed: Duration) -> LogSample {
    sample_live(cpu, io, elapsed).log
}

pub fn snap(seconds: u64) -> LogSample {
    let mut cpu = CpuSampler::new();
    cpu.prime();
    let mut io = IoAggregator::default();
    io.prime(&read_all_procs());
    let started = Instant::now();
    thread::sleep(Duration::from_secs(seconds.max(1)));
    sample_once(&mut cpu, &mut io, started.elapsed())
}

pub fn watch_loop(interval_secs: u64) -> ! {
    let interval = Duration::from_secs(interval_secs.max(1));
    let path = logfmt::default_log_path();
    let mut cpu = CpuSampler::new();
    cpu.prime();
    let mut io = IoAggregator::default();
    io.prime(&read_all_procs());
    eprintln!(
        "hdd-watch: sampling every {}s into {}",
        interval.as_secs(),
        path.display()
    );
    loop {
        let started = Instant::now();
        thread::sleep(interval);
        let sample = sample_once(&mut cpu, &mut io, started.elapsed());
        if let Err(err) = logfmt::append_sample(&path, &sample) {
            eprintln!("hdd-watch: cannot append to {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_parser_reads_some_avg10() {
        let content = "some avg10=12.34 avg60=5.00 avg300=1.00 total=123\nfull avg10=2.00 avg60=1.00 avg300=0.00 total=9\n";
        assert_eq!(parse_psi_avg10(content), Some(12.34));
        assert_eq!(
            parse_psi_avg10("some avg10=0.00 avg60=0.00 avg300=0.00 total=0"),
            Some(0.0)
        );
        assert_eq!(parse_psi_avg10("nonsense"), None);
    }

    #[test]
    fn meminfo_parser() {
        let info = parse_meminfo(
            "MemTotal:       16384000 kB\nMemAvailable:    4096000 kB\nSwapTotal:       2097152 kB\nSwapFree:        1048576 kB\n",
        );
        assert_eq!(info.total_kb, 16_384_000);
        assert_eq!(info.available_kb, 4_096_000);
        assert_eq!(info.swap_total_kb, 2_097_152);
        assert_eq!(info.swap_free_kb, 1_048_576);
    }

    #[test]
    fn cpu_parser_sums_fields_and_reads_iowait() {
        let times =
            parse_cpu_line("cpu  100 20 30 400 50 10 5 2 0 0\ncpu0 1 2 3 4 5 6 7 8").expect("cpu");
        assert_eq!(times.iowait, 50);
        assert_eq!(times.total, 617);
    }

    #[allow(clippy::too_many_arguments)]
    fn stat_line(
        pid: u32,
        comm: &str,
        state: char,
        ppid: u32,
        pgrp: u32,
        utime: u64,
        stime: u64,
        majflt: u64,
        delayacct: u64,
    ) -> String {
        let mut fields = vec!["0".to_string(); 40];
        fields[0] = state.to_string();
        fields[1] = ppid.to_string();
        fields[2] = pgrp.to_string();
        fields[9] = majflt.to_string();
        fields[11] = utime.to_string();
        fields[12] = stime.to_string();
        fields[19] = "1234".to_string();
        fields[39] = delayacct.to_string();
        format!("{pid} ({comm}) {}", fields.join(" "))
    }

    #[test]
    fn stat_parser_handles_parens_and_spaces_in_comm() {
        let line = stat_line(4242, "my app (v2)", 'D', 1, 4242, 10, 20, 7, 555);
        let stat = parse_stat_line(&line).expect("stat");
        assert_eq!(stat.pid, 4242);
        assert_eq!(stat.name, "my app (v2)");
        assert_eq!(stat.state, 'D');
        assert_eq!(stat.ppid, 1);
        assert_eq!(stat.pgrp, 4242);
        assert_eq!(stat.utime, 10);
        assert_eq!(stat.stime, 20);
        assert_eq!(stat.majflt, 7);
        assert_eq!(stat.delayacct, 555);
        assert_eq!(stat.starttime, 1234);
    }

    #[test]
    fn stat_parser_rejects_truncated_lines() {
        assert!(parse_stat_line("1 (x) S 1").is_none());
        assert!(parse_stat_line("").is_none());
    }

    #[test]
    fn proc_io_parser() {
        let io =
            parse_proc_io("rchar: 100\nwchar: 200\nsyscr: 1\nread_bytes: 300\nwrite_bytes: 400\n")
                .expect("io");
        assert_eq!(io.rchar, 100);
        assert_eq!(io.read_bytes, 300);
        assert_eq!(io.write_bytes, 400);
        assert!(parse_proc_io("nothing").is_none());
    }
}
