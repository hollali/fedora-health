use std::collections::HashMap;
use std::fmt::Write as _;

use crate::logfmt::LogSample;

pub const IO_PSI_WARN: f64 = 10.0;
pub const IO_PSI_BAD: f64 = 30.0;
pub const IOWAIT_WARN: f64 = 20.0;
pub const IOWAIT_BAD: f64 = 40.0;
pub const MEM_PSI_WARN: f64 = 10.0;
pub const MEM_PSI_BAD: f64 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Bad,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Bad => "BAD",
        }
    }
}

pub fn io_psi_severity(value: f64) -> Severity {
    if value >= IO_PSI_BAD {
        Severity::Bad
    } else if value >= IO_PSI_WARN {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

pub fn iowait_severity(value: f64) -> Severity {
    if value >= IOWAIT_BAD {
        Severity::Bad
    } else if value >= IOWAIT_WARN {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

pub fn mem_psi_severity(value: f64) -> Severity {
    if value >= MEM_PSI_BAD {
        Severity::Bad
    } else if value >= MEM_PSI_WARN {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

pub fn free_ram_severity(mb: u64) -> Severity {
    if mb < 512 {
        Severity::Bad
    } else if mb < 1024 {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

pub fn swap_severity(mb: u64) -> Severity {
    if mb >= 2000 {
        Severity::Bad
    } else if mb >= 500 {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trend {
    pub first_io_psi: f64,
    pub last_io_psi: f64,
    pub io_change_pct: f64,
    pub first_iowait: f64,
    pub last_iowait: f64,
    pub iowait_change_pct: f64,
    pub first_free_ram_mb: f64,
    pub last_free_ram_mb: f64,
    pub free_ram_change_pct: f64,
    pub first_count: usize,
    pub last_count: usize,
    pub degrading: bool,
}

fn percent_change(first: f64, last: f64) -> f64 {
    if first.abs() < f64::EPSILON {
        if last.abs() < f64::EPSILON {
            0.0
        } else {
            100.0
        }
    } else {
        ((last - first) / first) * 100.0
    }
}

pub fn calculate_trend(samples: &[LogSample]) -> Option<Trend> {
    if samples.len() < 6 {
        return None;
    }
    let third = samples.len() / 3;
    let first = &samples[..third];
    let last = &samples[samples.len().saturating_sub(third)..];
    let avg = |slice: &[LogSample], pick: fn(&LogSample) -> f64| -> f64 {
        if slice.is_empty() {
            0.0
        } else {
            slice.iter().map(pick).sum::<f64>() / slice.len() as f64
        }
    };
    let first_io = avg(first, |s| s.io_psi);
    let last_io = avg(last, |s| s.io_psi);
    let first_iowait = avg(first, |s| s.iowait_pct);
    let last_iowait = avg(last, |s| s.iowait_pct);
    let first_ram = avg(first, |s| s.mem_avail_mb as f64);
    let last_ram = avg(last, |s| s.mem_avail_mb as f64);
    let io_change = percent_change(first_io, last_io);
    let iowait_change = percent_change(first_iowait, last_iowait);
    let ram_change = percent_change(first_ram, last_ram);
    let degrading = (io_change >= 25.0 && last_io >= 1.0)
        || (iowait_change >= 25.0 && last_iowait >= 1.0)
        || (ram_change <= -10.0);
    Some(Trend {
        first_io_psi: first_io,
        last_io_psi: last_io,
        io_change_pct: io_change,
        first_iowait,
        last_iowait,
        iowait_change_pct: iowait_change,
        first_free_ram_mb: first_ram,
        last_free_ram_mb: last_ram,
        free_ram_change_pct: ram_change,
        first_count: first.len(),
        last_count: last.len(),
        degrading,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Culprit {
    pub name: String,
    pub total_kbps: f64,
    pub peak_kbps: f64,
    pub samples: usize,
}

impl Culprit {
    pub fn average_kbps(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.total_kbps / self.samples as f64
        }
    }
}

pub fn culprits(samples: &[LogSample]) -> Vec<Culprit> {
    let mut map: HashMap<String, (f64, f64, usize)> = HashMap::new();
    for sample in samples {
        if sample.io_psi < IO_PSI_WARN && sample.iowait_pct < IOWAIT_WARN {
            continue;
        }
        for proc in &sample.procs {
            let entry = map.entry(proc.name.clone()).or_insert((0.0, 0.0, 0));
            entry.0 += proc.kbps;
            entry.1 = entry.1.max(proc.kbps);
            entry.2 = entry.2.saturating_add(1);
        }
    }
    let mut out: Vec<Culprit> = map
        .into_iter()
        .map(|(name, (total, peak, count))| Culprit {
            name,
            total_kbps: total,
            peak_kbps: peak,
            samples: count,
        })
        .collect();
    out.sort_by(|a, b| b.total_kbps.total_cmp(&a.total_kbps));
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorstMoment {
    pub index: usize,
    pub minutes_ago: u64,
    pub io_psi: f64,
    pub mem_psi: f64,
    pub iowait_pct: f64,
    pub top_process: Option<(String, f64)>,
}

pub fn worst_moments(samples: &[LogSample], now: u64, limit: usize) -> Vec<WorstMoment> {
    let mut ranked: Vec<(usize, f64)> = samples
        .iter()
        .enumerate()
        .map(|(idx, sample)| {
            (
                idx,
                sample.io_psi + sample.iowait_pct * 0.5 + sample.mem_psi * 0.5,
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked
        .into_iter()
        .take(limit)
        .map(|(idx, _)| {
            let sample = &samples[idx];
            WorstMoment {
                index: idx,
                minutes_ago: now.saturating_sub(sample.ts) / 60,
                io_psi: sample.io_psi,
                mem_psi: sample.mem_psi,
                iowait_pct: sample.iowait_pct,
                top_process: sample
                    .procs
                    .first()
                    .map(|proc| (proc.name.clone(), proc.kbps)),
            }
        })
        .collect()
}

pub fn swap_thrash_hint(samples: &[LogSample]) -> Option<String> {
    let last = samples
        .iter()
        .rev()
        .find(|sample| sample.swap_used_mb > 500 && sample.mem_psi > MEM_PSI_WARN)?;
    Some(format!(
        "swap thrashing: {} MB swapped out with memory pressure {:.1}% — reduce memory pressure or add RAM/zram",
        last.swap_used_mb, last.mem_psi
    ))
}

pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {secs}s")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

fn trend_line(label: &str, first: f64, last: f64, change: f64, unit: &str) -> String {
    format!("  {label:<12} {first:>8.1}{unit} -> {last:>8.1}{unit}  ({change:+.1}%)")
}

pub fn render_trend(trend: &Trend) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "trend (first {} vs last {} samples):",
        trend.first_count, trend.last_count
    );
    let _ = writeln!(
        out,
        "{}",
        trend_line(
            "io_psi",
            trend.first_io_psi,
            trend.last_io_psi,
            trend.io_change_pct,
            "%"
        )
    );
    let _ = writeln!(
        out,
        "{}",
        trend_line(
            "iowait",
            trend.first_iowait,
            trend.last_iowait,
            trend.iowait_change_pct,
            "%"
        )
    );
    let _ = writeln!(
        out,
        "{}",
        trend_line(
            "free RAM",
            trend.first_free_ram_mb,
            trend.last_free_ram_mb,
            trend.free_ram_change_pct,
            "MB"
        )
    );
    let _ = writeln!(
        out,
        "  conclusion: {}",
        if trend.degrading {
            "DEGRADING over time"
        } else {
            "stable (no clear degradation)"
        }
    );
    out
}

pub fn render_report(samples: &[LogSample], now: u64, log_path: &str) -> String {
    if samples.is_empty() {
        return format!(
            "No samples found in {log_path}.\nStart recording with: sudo hdd-watch watch 10\n"
        );
    }
    let mut out = String::new();
    let first = &samples[0];
    let last = &samples[samples.len() - 1];
    let span = last.ts.saturating_sub(first.ts);
    let _ = writeln!(
        out,
        "hdd-watch report — {} samples over {} ({log_path})",
        samples.len(),
        format_duration(span)
    );
    let count = samples.len().max(1) as f64;
    let avg_io = samples.iter().map(|s| s.io_psi).sum::<f64>() / count;
    let avg_mem = samples.iter().map(|s| s.mem_psi).sum::<f64>() / count;
    let avg_iowait = samples.iter().map(|s| s.iowait_pct).sum::<f64>() / count;
    let _ = writeln!(
        out,
        "average: io_psi {avg_io:.1}%  mem_psi {avg_mem:.1}%  iowait {avg_iowait:.1}%"
    );
    let min_free = samples.iter().map(|s| s.mem_avail_mb).min().unwrap_or(0);
    let max_swap = samples.iter().map(|s| s.swap_used_mb).max().unwrap_or(0);
    let _ = writeln!(
        out,
        "min free RAM: {min_free} MB   max swap used: {max_swap} MB"
    );

    let _ = writeln!(out, "\nworst moments:");
    for (idx, moment) in worst_moments(samples, now, 5).iter().enumerate() {
        let top = moment
            .top_process
            .as_ref()
            .map(|(name, kbps)| format!("{name} {kbps:.0} KB/s"))
            .unwrap_or_else(|| "no process data".to_string());
        let _ = writeln!(
            out,
            "  {}. {} min ago  io {:.1}%  iowait {:.1}%  mem {:.1}%  top: {top}",
            idx + 1,
            moment.minutes_ago,
            moment.io_psi,
            moment.iowait_pct,
            moment.mem_psi
        );
    }

    let culprits = culprits(samples);
    let _ = writeln!(
        out,
        "\nprocesses during pressure (io_psi >= {IO_PSI_WARN:.0} or iowait >= {IOWAIT_WARN:.0}):"
    );
    if culprits.is_empty() {
        let _ = writeln!(out, "  (none recorded)");
    } else {
        let _ = writeln!(
            out,
            "  {:<28} {:>10} {:>10} {:>8}",
            "NAME", "AVG KB/s", "PEAK", "SAMPLES"
        );
        for culprit in culprits.iter().take(10) {
            let _ = writeln!(
                out,
                "  {:<28} {:>10.0} {:>10.0} {:>8}",
                truncate(&culprit.name, 28),
                culprit.average_kbps(),
                culprit.peak_kbps,
                culprit.samples
            );
        }
    }

    if let Some(hint) = swap_thrash_hint(samples) {
        let _ = writeln!(out, "\n{hint}");
    }

    let _ = writeln!(out);
    match calculate_trend(samples) {
        Some(trend) => out.push_str(&render_trend(&trend)),
        None => {
            let _ = writeln!(
                out,
                "trend: need at least 6 samples to compare the first and last third."
            );
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logfmt::ProcKbps;

    fn sample(ts: u64, io: f64, mem: f64, iowait: f64, ram: u64, swap: u64) -> LogSample {
        LogSample {
            ts,
            io_psi: io,
            mem_psi: mem,
            iowait_pct: iowait,
            mem_avail_mb: ram,
            swap_used_mb: swap,
            procs: vec![],
        }
    }

    #[test]
    fn severity_thresholds() {
        assert_eq!(io_psi_severity(9.99), Severity::Ok);
        assert_eq!(io_psi_severity(10.0), Severity::Warn);
        assert_eq!(io_psi_severity(30.0), Severity::Bad);
        assert_eq!(iowait_severity(20.0), Severity::Warn);
        assert_eq!(iowait_severity(40.0), Severity::Bad);
        assert_eq!(mem_psi_severity(10.0), Severity::Warn);
        assert_eq!(mem_psi_severity(30.0), Severity::Bad);
        assert_eq!(free_ram_severity(1024), Severity::Ok);
        assert_eq!(free_ram_severity(700), Severity::Warn);
        assert_eq!(free_ram_severity(100), Severity::Bad);
        assert_eq!(swap_severity(499), Severity::Ok);
        assert_eq!(swap_severity(500), Severity::Warn);
        assert_eq!(swap_severity(2000), Severity::Bad);
    }

    #[test]
    fn trend_detects_degradation() {
        let mut samples = Vec::new();
        for i in 0..9 {
            let io = if i < 3 {
                2.0
            } else if i < 6 {
                5.0
            } else {
                12.0
            };
            let ram = if i < 3 {
                3000
            } else if i < 6 {
                2400
            } else {
                1500
            };
            samples.push(sample(i, io, 0.0, 1.0, ram, 0));
        }
        let trend = calculate_trend(&samples).expect("trend");
        assert_eq!(trend.first_count, 3);
        assert_eq!(trend.last_count, 3);
        assert!(trend.io_change_pct > 25.0);
        assert!(trend.free_ram_change_pct < -10.0);
        assert!(trend.degrading);
    }

    #[test]
    fn trend_stable_data_is_not_degrading() {
        let samples: Vec<LogSample> = (0..9).map(|i| sample(i, 4.0, 0.0, 3.0, 2000, 0)).collect();
        let trend = calculate_trend(&samples).expect("trend");
        assert!(!trend.degrading);
        assert_eq!(trend.io_change_pct, 0.0);
    }

    #[test]
    fn trend_needs_three_samples() {
        assert!(calculate_trend(&[]).is_none());
        assert!(calculate_trend(&[sample(0, 1.0, 0.0, 0.0, 1, 0)]).is_none());
        assert!(calculate_trend(&vec![sample(0, 1.0, 0.0, 0.0, 1, 0); 5]).is_none());
        assert!(calculate_trend(&vec![sample(0, 1.0, 0.0, 0.0, 1, 0); 6]).is_some());
    }

    #[test]
    fn culprits_only_count_pressure_samples() {
        let mut a = sample(1, 20.0, 0.0, 0.0, 1000, 0);
        a.procs = vec![
            ProcKbps {
                name: "firefox".into(),
                kbps: 100.0,
            },
            ProcKbps {
                name: "bash".into(),
                kbps: 10.0,
            },
        ];
        let mut b = sample(2, 0.0, 0.0, 0.0, 1000, 0);
        b.procs = vec![ProcKbps {
            name: "firefox".into(),
            kbps: 500.0,
        }];
        let mut c = sample(3, 0.0, 0.0, 25.0, 1000, 0);
        c.procs = vec![ProcKbps {
            name: "firefox".into(),
            kbps: 50.0,
        }];
        let list = culprits(&[a, b, c]);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "firefox");
        assert_eq!(list[0].samples, 2);
        assert_eq!(list[0].peak_kbps, 100.0);
    }

    #[test]
    fn worst_moments_and_swap_hint() {
        let mut samples = vec![
            sample(100, 1.0, 0.0, 0.0, 1000, 100),
            sample(200, 40.0, 15.0, 30.0, 500, 800),
            sample(300, 2.0, 0.0, 0.0, 1000, 100),
        ];
        samples[1].procs = vec![ProcKbps {
            name: "updatedb".into(),
            kbps: 999.0,
        }];
        let worst = worst_moments(&samples, 360, 5);
        assert_eq!(worst[0].index, 1);
        assert_eq!(worst[0].minutes_ago, 2);
        assert_eq!(worst[0].top_process.as_ref().unwrap().0, "updatedb");
        assert!(swap_thrash_hint(&samples).is_some());
        assert!(swap_thrash_hint(&samples[..1]).is_none());
    }

    #[test]
    fn report_renders_all_sections() {
        let samples: Vec<LogSample> = (0..9)
            .map(|i| sample(100 + i * 10, i as f64, 1.0, i as f64, 2000, 100))
            .collect();
        let text = render_report(&samples, 200, "/tmp/x.log");
        assert!(text.contains("samples over"));
        assert!(text.contains("worst moments"));
        assert!(text.contains("processes during pressure"));
        assert!(text.contains("trend"));
    }
}
