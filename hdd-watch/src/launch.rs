use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::logfmt::{self, LaunchRecord};
use crate::sampler;

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub cold: bool,
    pub runs: u32,
    pub keep: bool,
    pub trace: bool,
    pub after_boot: bool,
    pub timeout_secs: u64,
    pub command: Vec<String>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            cold: false,
            runs: 1,
            keep: false,
            trace: false,
            after_boot: false,
            timeout_secs: 60,
            command: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub phase: String,
    pub elapsed_ms: u64,
    pub cpu_ms: u64,
    pub read_bytes: u64,
    pub majflt: u64,
    pub procs: u64,
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    DiskBound,
    CpuBound,
    Waiting,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DiskBound => "disk-bound",
            Self::CpuBound => "cpu-bound",
            Self::Waiting => "waiting",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Self::DiskBound => {
                "disk-bound: high D/block-I/O share or many major faults with low CPU use — prewarming and defragmentation help."
            }
            Self::CpuBound => {
                "CPU-bound: CPU time dominates; the disk is not the main bottleneck."
            }
            Self::Waiting => {
                "waiting on something else: low CPU and disk I/O while time passes — check DNS, D-Bus, keyring or a network call."
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchMetrics {
    pub wall_ms: u64,
    pub cpu_ms: u64,
    pub read_bytes: u64,
    pub majflt: u64,
    pub d_pct: f64,
    pub blkio_pct: f64,
    pub procs: u64,
}

#[derive(Clone, Debug)]
pub struct BenchResult {
    pub record: LaunchRecord,
    pub verdict: Verdict,
    pub window_ms: Option<u64>,
    pub timed_out: bool,
    pub trace_dirs: Vec<(String, u64)>,
}

#[derive(Clone, Debug, Default)]
pub struct LaunchReport {
    pub results: Vec<BenchResult>,
    pub already_running: Option<String>,
    pub cold_warm_ratio: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activity {
    pub t_ms: u64,
    pub cpu_ticks: u64,
    pub read_bytes: u64,
    pub in_d: bool,
    pub delayacct: u64,
    pub majflt: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    Settled(u64),
    NotSettled,
}

pub fn detect_settle(
    samples: &[Activity],
    quiet_ms: u64,
    max_cpu_ticks_per_sec: f64,
    max_io_bytes_per_sec: f64,
    timeout_ms: u64,
) -> Settle {
    if samples.len() < 2 {
        return Settle::NotSettled;
    }
    let mut quiet_since: Option<u64> = None;
    for pair in samples.windows(2) {
        let prev = pair[0];
        let cur = pair[1];
        if cur.t_ms >= timeout_ms {
            return Settle::NotSettled;
        }
        let dt = cur.t_ms.saturating_sub(prev.t_ms);
        if dt == 0 {
            continue;
        }
        let cpu_delta = cur.cpu_ticks.saturating_sub(prev.cpu_ticks);
        let io_delta = cur.read_bytes.saturating_sub(prev.read_bytes);
        let cpu_rate = cpu_delta as f64 * 1000.0 / dt as f64;
        let io_rate = io_delta as f64 * 1000.0 / dt as f64;
        let quiet = !prev.in_d
            && !cur.in_d
            && cpu_rate <= max_cpu_ticks_per_sec
            && io_rate <= max_io_bytes_per_sec;
        if quiet {
            let since = *quiet_since.get_or_insert(prev.t_ms);
            if cur.t_ms.saturating_sub(since) >= quiet_ms {
                return Settle::Settled(cur.t_ms);
            }
        } else {
            quiet_since = None;
        }
    }
    Settle::NotSettled
}

pub fn classify(metrics: &BenchMetrics) -> Verdict {
    let wall = metrics.wall_ms.max(1) as f64;
    let cpu_share = metrics.cpu_ms as f64 / wall * 100.0;
    let diskish = metrics.d_pct >= 15.0
        || metrics.blkio_pct >= 15.0
        || (metrics.majflt >= 100 && cpu_share < 50.0)
        || (metrics.read_bytes >= 4 * 1024 * 1024 && cpu_share < 50.0);
    if diskish {
        Verdict::DiskBound
    } else if cpu_share >= 60.0 {
        Verdict::CpuBound
    } else {
        Verdict::Waiting
    }
}

pub fn app_name(command: &[String]) -> String {
    command
        .first()
        .map(|arg| {
            Path::new(arg)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(arg)
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn drop_caches() -> Result<(), String> {
    if !logfmt::is_root() {
        return Err("--cold requires root (it drops the page cache for the whole machine)".into());
    }
    let status = Command::new("sync")
        .status()
        .map_err(|err| format!("cannot run sync: {err}"))?;
    if !status.success() {
        return Err("sync failed".into());
    }
    fs::write("/proc/sys/vm/drop_caches", "3")
        .map_err(|err| format!("cannot drop caches: {err}"))?;
    Ok(())
}

fn strace_available() -> bool {
    Command::new("strace")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn preexisting_names(name: &str) -> HashSet<u32> {
    sampler::read_all_procs()
        .into_iter()
        .filter(|proc| proc.name == name)
        .map(|proc| proc.pid)
        .collect()
}

fn group_activity(pgid: u32, seen: &mut HashSet<u32>) -> Activity {
    let procs = sampler::read_all_procs();
    let mut cpu_ticks = 0u64;
    let mut read_bytes = 0u64;
    let mut in_d = false;
    let mut delayacct = 0u64;
    let mut majflt = 0u64;
    for proc in sampler::group_members(&procs, pgid) {
        seen.insert(proc.pid);
        cpu_ticks = cpu_ticks
            .saturating_add(proc.utime)
            .saturating_add(proc.stime);
        majflt = majflt.saturating_add(proc.majflt);
        delayacct = delayacct.saturating_add(proc.delayacct);
        if proc.state == 'D' {
            in_d = true;
        }
        if let Some(io) = sampler::read_proc_io(proc.pid) {
            read_bytes = read_bytes.saturating_add(io.read_bytes);
        }
    }
    Activity {
        t_ms: 0,
        cpu_ticks,
        read_bytes,
        in_d,
        delayacct,
        majflt,
    }
}

fn group_alive(pgid: u32) -> bool {
    !sampler::group_members(&sampler::read_all_procs(), pgid).is_empty()
}

fn terminate_group(pgid: u32, child: &mut Child) {
    let _ = child.try_wait();
    if !group_alive(pgid) {
        return;
    }
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(format!("-{pgid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let _ = child.try_wait();
        if !group_alive(pgid) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pgid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.wait();
}

struct WindowWatcher {
    next_check: u64,
    found: Option<u64>,
    enabled: bool,
}

impl WindowWatcher {
    fn new() -> Self {
        let enabled = std::env::var_os("DISPLAY").is_some();
        Self {
            next_check: 0,
            found: None,
            enabled,
        }
    }

    fn poll(&mut self, pid: u32, elapsed_ms: u64) {
        if self.found.is_some() || !self.enabled || elapsed_ms < self.next_check {
            return;
        }
        self.next_check = elapsed_ms.saturating_add(500);
        let Ok(output) = Command::new("xdotool")
            .args(["search", "--pid", &pid.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        else {
            return;
        };
        if output.status.success() && !output.stdout.is_empty() {
            self.found = Some(elapsed_ms);
        }
    }
}

fn bench_one(
    command: &[String],
    app: &str,
    kind: &str,
    opts: &LaunchOptions,
    phase: &str,
    progress: &mut dyn FnMut(Progress),
) -> Result<Option<BenchResult>, String> {
    let trace_file = if opts.trace {
        if !strace_available() {
            eprintln!("hdd-watch: strace is not installed; skipping --trace");
        }
        Some(std::env::temp_dir().join(format!(
            "hdd-watch-trace-{}-{}.log",
            std::process::id(),
            logfmt::now_epoch()
        )))
    } else {
        None
    };
    let use_strace = trace_file.is_some() && strace_available();

    let mut argv: Vec<String> = Vec::new();
    if use_strace {
        let trace_path = trace_file.as_ref().map(|p| p.to_string_lossy().to_string());
        if let Some(trace_path) = trace_path {
            argv.extend([
                "-f".to_string(),
                "-e".to_string(),
                "trace=openat,execve".to_string(),
                "-tt".to_string(),
                "-o".to_string(),
                trace_path,
                "--".to_string(),
            ]);
        }
    }
    argv.extend(command.iter().cloned());
    let program = if use_strace {
        "strace".to_string()
    } else {
        command
            .first()
            .cloned()
            .ok_or_else(|| "no command given".to_string())?
    };
    let args = if use_strace {
        argv
    } else {
        argv.into_iter().skip(1).collect()
    };

    let preexisting = preexisting_names(app);
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|err| format!("cannot start {program}: {err}"))?;
    let pgid = child.id();

    let started = Instant::now();
    let timeout_ms = opts.timeout_secs.max(1).saturating_mul(1000);
    let mut samples: Vec<Activity> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut watcher = WindowWatcher::new();
    let mut settled_ms: Option<u64> = None;
    let mut timed_out = false;
    let mut root_exited_ms: Option<u64> = None;

    loop {
        thread::sleep(Duration::from_millis(100));
        let elapsed = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let mut activity = group_activity(pgid, &mut seen);
        activity.t_ms = elapsed;
        let alive = !sampler::group_members(&sampler::read_all_procs(), pgid).is_empty();
        watcher.poll(pgid, elapsed);
        let cpu_ms = activity.cpu_ticks.saturating_mul(10);
        progress(Progress {
            phase: phase.to_string(),
            elapsed_ms: elapsed,
            cpu_ms,
            read_bytes: activity.read_bytes,
            majflt: activity.majflt,
            procs: seen.len() as u64,
            settled: false,
        });
        samples.push(activity);
        if let Ok(Some(_status)) = child.try_wait() {
            if root_exited_ms.is_none() {
                root_exited_ms = Some(elapsed);
            }
        }
        if !alive {
            break;
        }
        if elapsed >= timeout_ms {
            timed_out = true;
            break;
        }
    }

    if !opts.keep {
        terminate_group(pgid, &mut child);
    } else {
        let _ = child.try_wait();
    }

    if let Some(exit_ms) = root_exited_ms {
        if exit_ms <= 1000 && seen.len() <= 1 && !preexisting.is_empty() {
            if let Some(path) = &trace_file {
                let _ = fs::remove_file(path);
            }
            return Ok(None);
        }
    }

    let settle = detect_settle(&samples, 1500, 3.0, 32_768.0, timeout_ms);
    if let Settle::Settled(at) = settle {
        settled_ms = Some(at);
    }
    let last = samples.last().copied().unwrap_or(Activity {
        t_ms: 0,
        cpu_ticks: 0,
        read_bytes: 0,
        in_d: false,
        delayacct: 0,
        majflt: 0,
    });
    let first = samples.first().copied().unwrap_or(last);
    if settled_ms.is_none() && samples.len() >= 2 && !timed_out {
        settled_ms = Some(last.t_ms);
    }
    let wall_ms = settled_ms.unwrap_or(last.t_ms).max(1);
    let d_samples = samples.iter().filter(|sample| sample.in_d).count();
    let d_pct = if samples.is_empty() {
        0.0
    } else {
        d_samples as f64 / samples.len() as f64 * 100.0
    };
    let delay_delta = last.delayacct.saturating_sub(first.delayacct);
    let blkio_pct = ((delay_delta.saturating_mul(10) as f64 / wall_ms as f64) * 100.0).min(100.0);
    let metrics = BenchMetrics {
        wall_ms,
        cpu_ms: last.cpu_ticks.saturating_mul(10),
        read_bytes: last.read_bytes,
        majflt: last.majflt,
        d_pct,
        blkio_pct,
        procs: seen.len() as u64,
    };
    let verdict = classify(&metrics);

    let mut trace_dirs = Vec::new();
    if let Some(trace_path) = trace_file {
        if use_strace {
            if let Ok(content) = fs::read_to_string(&trace_path) {
                let summary = parse_trace(&content);
                trace_dirs = summary.directories.clone();
                save_trace_files(app, &summary.files);
                print_trace_summary(&summary);
            }
        }
        let _ = fs::remove_file(&trace_path);
    }

    let record = LaunchRecord {
        ts: logfmt::now_epoch(),
        app: app.to_string(),
        kind: kind.to_string(),
        settled_ms: settled_ms.unwrap_or(wall_ms),
        cpu_ms: metrics.cpu_ms,
        read_bytes: metrics.read_bytes,
        majflt: metrics.majflt,
        d_pct: metrics.d_pct,
        blkio_pct: metrics.blkio_pct,
        procs: metrics.procs,
        verdict: verdict.as_str().to_string(),
        command: command.join(" "),
    };
    if let Err(err) = logfmt::append_launch(&logfmt::launches_path(), &record) {
        eprintln!("hdd-watch: could not record launch: {err}");
    }
    let _ = child.wait();
    Ok(Some(BenchResult {
        record,
        verdict,
        window_ms: watcher.found,
        timed_out,
        trace_dirs,
    }))
}

pub fn run(
    opts: &LaunchOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<LaunchReport, String> {
    if opts.command.is_empty() {
        return Err("no command given; use: hdd-watch launch -- <command...>".into());
    }
    if opts.cold && !logfmt::is_root() {
        return Err("--cold requires root (it drops the page cache for the whole machine)".into());
    }
    let app = app_name(&opts.command);
    let kinds: Vec<&str> = if opts.cold {
        vec!["cold", "warm", "warm"]
    } else if opts.after_boot {
        vec!["after-boot"]
    } else {
        vec!["warm"; opts.runs.max(1) as usize]
    };
    let mut report = LaunchReport::default();
    for (idx, kind) in kinds.iter().enumerate() {
        if *kind == "cold" {
            drop_caches()?;
        }
        let phase = format!("{kind} {}/{}", idx + 1, kinds.len());
        match bench_one(&opts.command, &app, kind, opts, &phase, progress)? {
            Some(result) => report.results.push(result),
            None => {
                report.already_running = Some(app.clone());
                return Ok(report);
            }
        }
    }
    if opts.cold && report.results.len() >= 3 {
        let cold = report.results[0].record.settled_ms as f64;
        let warm: f64 = report.results[1..]
            .iter()
            .map(|r| r.record.settled_ms as f64)
            .sum::<f64>()
            / (report.results.len() - 1) as f64;
        if warm > 0.0 {
            report.cold_warm_ratio = Some(cold / warm);
        }
    }
    Ok(report)
}

#[derive(Clone, Debug, Default)]
pub struct TraceSummary {
    pub directories: Vec<(String, u64)>,
    pub files: Vec<PathBuf>,
}

fn unescape_path(raw: &str) -> String {
    let mut out = String::new();
    let mut escaped = false;
    for ch in raw.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            out.push(ch);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

pub fn parse_trace(content: &str) -> TraceSummary {
    let mut files: HashSet<PathBuf> = HashSet::new();
    for line in content.lines() {
        let Some(start) = line.find("openat(").or_else(|| line.find("execve(")) else {
            continue;
        };
        let rest = &line[start..];
        let Some(first_quote) = rest.find('"') else {
            continue;
        };
        let after = &rest[first_quote + 1..];
        let mut raw = String::new();
        let mut escaped = false;
        for ch in after.chars() {
            if escaped {
                raw.push('\\');
                raw.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                break;
            } else {
                raw.push(ch);
            }
        }
        let path = PathBuf::from(unescape_path(&raw));
        if path.is_absolute() && path.exists() && !files.contains(&path) {
            files.insert(path);
        }
    }
    let mut dir_bytes: HashMap<String, u64> = HashMap::new();
    for file in &files {
        let size = fs::metadata(file).map(|meta| meta.len()).unwrap_or(0);
        let dir = file
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "/".to_string());
        let entry = dir_bytes.entry(dir).or_insert(0);
        *entry = entry.saturating_add(size);
    }
    let mut directories: Vec<(String, u64)> = dir_bytes.into_iter().collect();
    directories.sort_by_key(|a| std::cmp::Reverse(a.1));
    let mut files: Vec<PathBuf> = files.into_iter().collect();
    files.sort();
    TraceSummary { directories, files }
}

fn save_trace_files(app: &str, files: &[PathBuf]) {
    let dir = logfmt::state_dir().join("traces");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{}.files", logfmt::sanitize_name(app)));
    let mut out = String::new();
    for file in files {
        out.push_str(&file.display().to_string());
        out.push('\n');
    }
    if fs::write(&path, out).is_err() {
        eprintln!(
            "hdd-watch: could not save trace file list to {}",
            path.display()
        );
    }
}

fn print_trace_summary(summary: &TraceSummary) {
    println!("\ntrace: opened files grouped by directory (total size):");
    for (dir, bytes) in summary.directories.iter().take(15) {
        println!("  {:>9.1} MB  {dir}", *bytes as f64 / 1024.0 / 1024.0);
    }
    println!("  {} files; list saved for prewarm", summary.files.len());
}

pub fn load_history() -> Vec<LaunchRecord> {
    logfmt::read_launches(&logfmt::launches_path())
}

pub fn render_result(result: &BenchResult) {
    let record = &result.record;
    println!(
        "\n{} [{}] {}",
        record.app,
        record.kind,
        if result.timed_out {
            "TIMED OUT"
        } else {
            "settled"
        }
    );
    println!(
        "  time to settled: {:.2} s",
        record.settled_ms as f64 / 1000.0
    );
    if let Some(window_ms) = result.window_ms {
        println!("  first window:    {:.2} s", window_ms as f64 / 1000.0);
    }
    let cpu_share = if record.settled_ms == 0 {
        0.0
    } else {
        record.cpu_ms as f64 / record.settled_ms as f64 * 100.0
    };
    println!(
        "  CPU:             {:.2} s ({cpu_share:.0}% of wall)",
        record.cpu_ms as f64 / 1000.0
    );
    println!(
        "  read from disk:  {:.1} MB",
        record.read_bytes as f64 / 1024.0 / 1024.0
    );
    println!("  major faults:    {}", record.majflt);
    println!(
        "  D-state:         {:.0}%   block-I/O wait: {:.0}%",
        record.d_pct, record.blkio_pct
    );
    println!("  processes:       {}", record.procs);
    println!(
        "  verdict:         {} — {}",
        result.verdict.as_str(),
        result.verdict.explanation()
    );
}

pub fn print_report(report: &LaunchReport) {
    if let Some(app) = &report.already_running {
        println!(
            "{app} was already running (the new process forwarded to the existing instance and exited).\nClose the app first and run the benchmark again; the reported time would be misleading."
        );
        return;
    }
    for result in &report.results {
        render_result(result);
    }
    if let Some(ratio) = report.cold_warm_ratio {
        println!("\ncold/warm ratio: {ratio:.2}x");
        if ratio >= 2.0 {
            println!("  Big ratio: this is a disk read problem; prewarming should help.");
        } else {
            println!("  Ratio near 1: the delay is not mainly the disk.");
        }
    }
}

pub fn render_launches() -> String {
    let history = load_history();
    if history.is_empty() {
        return format!(
            "No launch history in {}.\nRun: hdd-watch launch [--cold] -- <command...>\n",
            logfmt::launches_path().display()
        );
    }
    let mut groups: HashMap<String, Vec<LaunchRecord>> = HashMap::new();
    for record in history {
        groups.entry(record.app.clone()).or_default().push(record);
    }
    let mut apps: Vec<(String, Vec<LaunchRecord>)> = groups.into_iter().collect();
    apps.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    out.push_str(&format!(
        "launch history — {} (first vs latest, cold vs warm)\n",
        logfmt::launches_path().display()
    ));
    for (app, mut records) in apps {
        records.sort_by_key(|record| record.ts);
        let first = records.first().map(|r| r.settled_ms).unwrap_or(0);
        let latest = records.last().map(|r| r.settled_ms).unwrap_or(0);
        let cold: Vec<u64> = records
            .iter()
            .filter(|r| r.kind == "cold")
            .map(|r| r.settled_ms)
            .collect();
        let warm: Vec<u64> = records
            .iter()
            .filter(|r| r.kind != "cold")
            .map(|r| r.settled_ms)
            .collect();
        let cold_avg = if cold.is_empty() {
            None
        } else {
            Some(cold.iter().sum::<u64>() as f64 / cold.len() as f64)
        };
        let warm_avg = if warm.is_empty() {
            None
        } else {
            Some(warm.iter().sum::<u64>() as f64 / warm.len() as f64)
        };
        let slower = latest > first.saturating_add(1000) && latest > first.saturating_mul(3) / 2;
        out.push_str(&format!(
            "\n{app}  ({} runs{})\n",
            records.len(),
            if slower { ", SLOWER over time" } else { "" }
        ));
        out.push_str(&format!(
            "  first {:.2}s -> latest {:.2}s\n",
            first as f64 / 1000.0,
            latest as f64 / 1000.0
        ));
        match (cold_avg, warm_avg) {
            (Some(c), Some(w)) if w > 0.0 => out.push_str(&format!(
                "  cold {:.2}s / warm {:.2}s = {:.2}x\n",
                c / 1000.0,
                w / 1000.0,
                c / w
            )),
            (Some(c), _) => out.push_str(&format!("  cold {:.2}s\n", c / 1000.0)),
            (_, Some(w)) => out.push_str(&format!("  warm {:.2}s\n", w / 1000.0)),
            _ => {}
        }
        if let Some(last) = records.last() {
            out.push_str(&format!(
                "  latest verdict: {} ({})\n",
                last.verdict,
                logfmt::sanitize_field(&last.command)
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(t_ms: u64, cpu: u64, read: u64, in_d: bool) -> Activity {
        Activity {
            t_ms,
            cpu_ticks: cpu,
            read_bytes: read,
            in_d,
            delayacct: 0,
            majflt: 0,
        }
    }

    #[test]
    fn settle_spiky_start_then_quiet() {
        let mut samples = Vec::new();
        for i in 0..=7 {
            samples.push(activity(i * 100, i * 20, i * 200_000, false));
        }
        for i in 8..=40 {
            samples.push(activity(i * 100, 140, 1_400_000, false));
        }
        let settle = detect_settle(&samples, 1500, 3.0, 32_768.0, 10_000);
        assert!(matches!(settle, Settle::Settled(ms) if (2_100..=2_500).contains(&ms)));
    }

    #[test]
    fn settle_long_quiet_tail() {
        let samples: Vec<Activity> = (0..=40).map(|i| activity(i * 100, 0, 0, false)).collect();
        assert_eq!(
            detect_settle(&samples, 1500, 3.0, 32_768.0, 10_000),
            Settle::Settled(1_500)
        );
    }

    #[test]
    fn settle_never_settles_times_out() {
        let samples: Vec<Activity> = (0..=100)
            .map(|i| activity(i * 100, i * 50, i * 500_000, false))
            .collect();
        assert_eq!(
            detect_settle(&samples, 1500, 3.0, 32_768.0, 10_000),
            Settle::NotSettled
        );
    }

    #[test]
    fn settle_d_state_blocks_quiet() {
        let samples: Vec<Activity> = (0..=40)
            .map(|i| activity(i * 100, 0, 0, i > 10 && i < 20))
            .collect();
        let result = detect_settle(&samples, 1500, 3.0, 32_768.0, 10_000);
        assert!(
            matches!(result, Settle::Settled(ms) if ms >= 3_500),
            "{result:?}"
        );
    }

    #[test]
    fn verdict_disk_bound() {
        let metrics = BenchMetrics {
            wall_ms: 5_000,
            cpu_ms: 500,
            read_bytes: 80 * 1024 * 1024,
            majflt: 400,
            d_pct: 40.0,
            blkio_pct: 30.0,
            procs: 3,
        };
        assert_eq!(classify(&metrics), Verdict::DiskBound);
    }

    #[test]
    fn verdict_cpu_bound() {
        let metrics = BenchMetrics {
            wall_ms: 2_000,
            cpu_ms: 1_800,
            read_bytes: 0,
            majflt: 0,
            d_pct: 0.0,
            blkio_pct: 0.0,
            procs: 1,
        };
        assert_eq!(classify(&metrics), Verdict::CpuBound);
    }

    #[test]
    fn verdict_waiting() {
        let metrics = BenchMetrics {
            wall_ms: 8_000,
            cpu_ms: 100,
            read_bytes: 100_000,
            majflt: 2,
            d_pct: 0.0,
            blkio_pct: 1.0,
            procs: 2,
        };
        assert_eq!(classify(&metrics), Verdict::Waiting);
    }

    #[test]
    fn trace_parser_groups_files_and_ignores_missing() {
        let content = r#"1700000000.000000 openat(AT_FDCWD, "/etc/passwd", O_RDONLY) = 3
1700000000.100000 openat(AT_FDCWD, "/etc/passwd", O_RDONLY) = 3
1700000000.200000 openat(AT_FDCWD, "/does/not/exist", O_RDONLY) = -1
1700000000.300000 execve("/usr/bin/ls", ["ls"], 0x7ffd) = 0
"#;
        let summary = parse_trace(content);
        assert_eq!(summary.files.len(), 2);
        assert!(summary.directories.iter().any(|(dir, _)| dir == "/etc"));
    }

    #[test]
    fn app_name_uses_basename() {
        assert_eq!(app_name(&["/usr/bin/code".into(), "--new".into()]), "code");
        assert_eq!(app_name(&[]), "unknown");
    }
}
