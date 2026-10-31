use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_LOG_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ProcKbps {
    pub name: String,
    pub kbps: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogSample {
    pub ts: u64,
    pub io_psi: f64,
    pub mem_psi: f64,
    pub iowait_pct: f64,
    pub mem_avail_mb: u64,
    pub swap_used_mb: u64,
    pub procs: Vec<ProcKbps>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaunchRecord {
    pub ts: u64,
    pub app: String,
    pub kind: String,
    pub settled_ms: u64,
    pub cpu_ms: u64,
    pub read_bytes: u64,
    pub majflt: u64,
    pub d_pct: f64,
    pub blkio_pct: f64,
    pub procs: u64,
    pub verdict: String,
    pub command: String,
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn sanitize_field(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            '|' | ';' | '=' | '\n' | '\r' | '\t' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

pub fn sanitize_name(input: &str) -> String {
    let cleaned = sanitize_field(input);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn format_sample(sample: &LogSample) -> String {
    let procs = sample
        .procs
        .iter()
        .map(|p| format!("{}={:.1}", sanitize_name(&p.name), p.kbps))
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "{}|{:.2}|{:.2}|{:.2}|{}|{}|{}",
        sample.ts,
        sample.io_psi,
        sample.mem_psi,
        sample.iowait_pct,
        sample.mem_avail_mb,
        sample.swap_used_mb,
        procs
    )
}

pub fn parse_sample(line: &str) -> Option<LogSample> {
    let parts: Vec<&str> = line.trim().split('|').collect();
    if parts.len() != 7 {
        return None;
    }
    let ts = parts[0].parse().ok()?;
    let io_psi = parts[1].parse().ok()?;
    let mem_psi = parts[2].parse().ok()?;
    let iowait_pct = parts[3].parse().ok()?;
    let mem_avail_mb = parts[4].parse().ok()?;
    let swap_used_mb = parts[5].parse().ok()?;
    let mut procs = Vec::new();
    for entry in parts[6].split(';').filter(|e| !e.is_empty()) {
        if let Some((name, value)) = entry.rsplit_once('=') {
            if let Ok(kbps) = value.parse::<f64>() {
                if !name.is_empty() {
                    procs.push(ProcKbps {
                        name: name.to_string(),
                        kbps,
                    });
                }
            }
        }
    }
    procs.sort_by(|a, b| b.kbps.total_cmp(&a.kbps));
    Some(LogSample {
        ts,
        io_psi,
        mem_psi,
        iowait_pct,
        mem_avail_mb,
        swap_used_mb,
        procs,
    })
}

pub fn format_launch(record: &LaunchRecord) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{:.2}|{:.2}|{}|{}|{}",
        record.ts,
        sanitize_field(&record.app),
        sanitize_field(&record.kind),
        record.settled_ms,
        record.cpu_ms,
        record.read_bytes,
        record.majflt,
        record.d_pct,
        record.blkio_pct,
        record.procs,
        sanitize_field(&record.verdict),
        sanitize_field(&record.command)
    )
}

pub fn parse_launch(line: &str) -> Option<LaunchRecord> {
    let parts: Vec<&str> = line.trim().splitn(12, '|').collect();
    if parts.len() != 12 {
        return None;
    }
    Some(LaunchRecord {
        ts: parts[0].parse().ok()?,
        app: parts[1].to_string(),
        kind: parts[2].to_string(),
        settled_ms: parts[3].parse().ok()?,
        cpu_ms: parts[4].parse().ok()?,
        read_bytes: parts[5].parse().ok()?,
        majflt: parts[6].parse().ok()?,
        d_pct: parts[7].parse().ok()?,
        blkio_pct: parts[8].parse().ok()?,
        procs: parts[9].parse().ok()?,
        verdict: parts[10].to_string(),
        command: parts[11].to_string(),
    })
}

pub fn is_root() -> bool {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return false;
    };
    status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .map(|uid| uid == "0")
        .unwrap_or(false)
}

#[derive(Clone, Debug)]
pub struct RealUser {
    pub name: String,
    pub uid: u32,
    pub home: PathBuf,
}

pub fn real_user() -> RealUser {
    let sudo_user = env::var("SUDO_USER").ok().filter(|s| !s.is_empty());
    let current_uid = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|uid| uid.parse::<u32>().ok())
        })
        .unwrap_or(1000);
    let current_name = env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "user".to_string());
    let current_home = env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home").join(&current_name));

    let target = sudo_user.as_deref().unwrap_or(&current_name);
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 6 && fields[0] == target {
            return RealUser {
                name: fields[0].to_string(),
                uid: fields[2].parse().unwrap_or(current_uid),
                home: PathBuf::from(fields[5]),
            };
        }
    }
    RealUser {
        name: target.to_string(),
        uid: current_uid,
        home: current_home,
    }
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = env::var("HDD_WATCH_STATE") {
        return PathBuf::from(dir);
    }
    if is_root() {
        return PathBuf::from("/var/lib/hdd-watch");
    }
    let user = real_user();
    if let Ok(xdg) = env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("hdd-watch");
        }
    }
    user.home.join(".local/state/hdd-watch")
}

pub fn default_log_path() -> PathBuf {
    if let Ok(path) = env::var("HDD_WATCH_LOG") {
        return PathBuf::from(path);
    }
    if is_root() {
        PathBuf::from("/var/log/hdd-watch.log")
    } else {
        state_dir().join("hdd-watch.log")
    }
}

pub fn launches_path() -> PathBuf {
    if let Ok(path) = env::var("HDD_WATCH_LAUNCHES") {
        return PathBuf::from(path);
    }
    state_dir().join("launches.log")
}

pub fn prewarm_conf_path() -> PathBuf {
    env::var("HDD_WATCH_PREWARM_CONF")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/hdd-watch/prewarm.conf"))
}

pub fn rotate_if_needed(path: &Path, max_bytes: u64) -> io::Result<bool> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(false);
    };
    if meta.len() < max_bytes {
        return Ok(false);
    }
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".1");
    let backup = PathBuf::from(backup);
    match fs::remove_file(&backup) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    fs::rename(path, &backup)?;
    Ok(true)
}

pub fn append_line(path: &Path, line: &str, max_bytes: u64) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    rotate_if_needed(path, max_bytes)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

pub fn append_sample(path: &Path, sample: &LogSample) -> io::Result<()> {
    append_line(path, &format_sample(sample), MAX_LOG_BYTES)
}

pub fn append_launch(path: &Path, record: &LaunchRecord) -> io::Result<()> {
    append_line(path, &format_launch(record), MAX_LOG_BYTES)
}

pub fn read_samples(path: &Path) -> Vec<LogSample> {
    fs::read_to_string(path)
        .map(|text| text.lines().filter_map(parse_sample).collect())
        .unwrap_or_default()
}

pub fn read_launches(path: &Path) -> Vec<LaunchRecord> {
    fs::read_to_string(path)
        .map(|text| text.lines().filter_map(parse_launch).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LogSample {
        LogSample {
            ts: 1_700_000_000,
            io_psi: 12.345,
            mem_psi: 1.5,
            iowait_pct: 22.0,
            mem_avail_mb: 4096,
            swap_used_mb: 128,
            procs: vec![
                ProcKbps {
                    name: "firefox".into(),
                    kbps: 1234.5,
                },
                ProcKbps {
                    name: "my|weird;name=x".into(),
                    kbps: 7.0,
                },
            ],
        }
    }

    #[test]
    fn sample_roundtrip_and_sanitise() {
        let line = format_sample(&sample());
        assert!(!line.contains("my|weird"));
        assert!(line.contains("my_weird_name_x=7.0"));
        let parsed = parse_sample(&line).expect("parse");
        assert_eq!(parsed.ts, 1_700_000_000);
        assert!((parsed.io_psi - 12.35).abs() < 0.001);
        assert_eq!(parsed.mem_avail_mb, 4096);
        assert_eq!(parsed.procs.len(), 2);
        assert_eq!(parsed.procs[0].name, "firefox");
    }

    #[test]
    fn parse_sample_rejects_bad_lines() {
        assert!(parse_sample("").is_none());
        assert!(parse_sample("1|2|3").is_none());
        assert!(parse_sample("x|2|3|4|5|6|").is_none());
        assert!(parse_sample("1|2|3|4|5|6|").is_some());
    }

    #[test]
    fn parse_sample_tolerates_bad_process_entries() {
        let parsed = parse_sample("1|2.0|3.0|4.0|5|6|good=1.0;bad;also=xx;empty=").expect("parse");
        assert_eq!(parsed.procs.len(), 1);
        assert_eq!(parsed.procs[0].name, "good");
    }

    #[test]
    fn launch_roundtrip_preserves_last_field() {
        let record = LaunchRecord {
            ts: 42,
            app: "code".into(),
            kind: "cold".into(),
            settled_ms: 4321,
            cpu_ms: 900,
            read_bytes: 123_456_789,
            majflt: 99,
            d_pct: 12.5,
            blkio_pct: 3.25,
            procs: 7,
            verdict: "disk-bound".into(),
            command: "code --new-window /tmp/a b".into(),
        };
        let line = format_launch(&record);
        let parsed = parse_launch(&line).expect("parse");
        assert_eq!(parsed.command, "code --new-window /tmp/a b");
        assert_eq!(parsed.verdict, "disk-bound");
        assert_eq!(parsed.settled_ms, 4321);
    }

    #[test]
    fn rotation_keeps_one_backup() {
        let dir = env::temp_dir().join(format!("hdd-watch-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("test.log");
        fs::write(&path, vec![b'x'; 64]).expect("write");
        let rotated = rotate_if_needed(&path, 32).expect("rotate");
        assert!(rotated);
        assert!(!path.exists());
        assert!(dir.join("test.log.1").exists());
        fs::write(&path, vec![b'y'; 64]).expect("write2");
        let rotated = rotate_if_needed(&path, 32).expect("rotate2");
        assert!(rotated);
        assert_eq!(fs::read(dir.join("test.log.1")).expect("read").len(), 64);
        let _ = fs::remove_dir_all(&dir);
    }
}
