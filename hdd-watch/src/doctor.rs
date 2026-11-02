use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::report::Severity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub fix_available: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse(&mut self) -> Result<Json, String> {
        self.skip_ws();
        let value = self.parse_value()?;
        self.skip_ws();
        if self.pos != self.bytes.len() {
            return Err(format!("trailing data at byte {}", self.pos));
        }
        Ok(value)
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_string().map(Json::String),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.parse_number(),
            Some(byte) => Err(format!(
                "unexpected byte '{}' at byte {}",
                byte as char, self.pos
            )),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn literal(&mut self, text: &str) -> Result<(), String> {
        if self
            .bytes
            .get(self.pos..self.pos.saturating_add(text.len()))
            == Some(text.as_bytes())
        {
            self.pos = self.pos.saturating_add(text.len());
            Ok(())
        } else {
            Err(format!("expected {text} at byte {}", self.pos))
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.pos = self.pos.saturating_add(1);
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos = self.pos.saturating_add(1);
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos = self.pos.saturating_add(1),
                Some(b']') => {
                    self.pos = self.pos.saturating_add(1);
                    return Ok(Json::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.pos = self.pos.saturating_add(1);
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos = self.pos.saturating_add(1);
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at byte {}", self.pos));
            }
            self.pos = self.pos.saturating_add(1);
            let value = self.parse_value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos = self.pos.saturating_add(1),
                Some(b'}') => {
                    self.pos = self.pos.saturating_add(1);
                    return Ok(Json::Object(fields));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("expected string at byte {}", self.pos));
        }
        self.pos = self.pos.saturating_add(1);
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err("unterminated string".to_string());
            };
            self.pos = self.pos.saturating_add(1);
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(escape) = self.peek() else {
                        return Err("unterminated escape".to_string());
                    };
                    self.pos = self.pos.saturating_add(1);
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                if self.bytes.get(self.pos..self.pos.saturating_add(2))
                                    != Some(b"\\u")
                                {
                                    return Err("unpaired surrogate".to_string());
                                }
                                self.pos = self.pos.saturating_add(2);
                                let low = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err("invalid low surrogate".to_string());
                                }
                                let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                let ch = char::from_u32(combined)
                                    .ok_or_else(|| "invalid unicode escape".to_string())?;
                                out.push(ch);
                            } else {
                                let ch = char::from_u32(code)
                                    .ok_or_else(|| "invalid unicode escape".to_string())?;
                                out.push(ch);
                            }
                        }
                        other => return Err(format!("invalid escape \\{}", other as char)),
                    }
                }
                byte if byte < 0x20 => {
                    return Err("control byte in string".to_string());
                }
                byte if byte < 0x80 => out.push(byte as char),
                _ => {
                    let start = self.pos.saturating_sub(1);
                    let width = utf8_width(byte);
                    let end = start.saturating_add(width);
                    let slice = self
                        .bytes
                        .get(start..end)
                        .ok_or_else(|| "truncated utf-8".to_string())?;
                    let text = std::str::from_utf8(slice)
                        .map_err(|_| "invalid utf-8 in string".to_string())?;
                    out.push_str(text);
                    self.pos = end;
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let slice = self
            .bytes
            .get(self.pos..self.pos.saturating_add(4))
            .ok_or_else(|| "truncated unicode escape".to_string())?;
        let text = std::str::from_utf8(slice).map_err(|_| "invalid unicode escape".to_string())?;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| "invalid unicode escape".to_string())?;
        self.pos = self.pos.saturating_add(4);
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while self.peek().is_some_and(|byte| {
            matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') || byte.is_ascii_digit()
        }) {
            self.pos = self.pos.saturating_add(1);
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| "invalid number".to_string())?;
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| format!("invalid number at byte {start}"))
    }
}

fn utf8_width(first: u8) -> usize {
    if first & 0b1110_0000 == 0b1100_0000 {
        2
    } else if first & 0b1111_0000 == 0b1110_0000 {
        3
    } else if first & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        1
    }
}

pub fn parse_findings(input: &str) -> Result<Vec<Finding>, String> {
    let json = Parser::new(input).parse()?;
    let Json::Array(items) = json else {
        return Err("doctor JSON is not an array".to_string());
    };
    let mut findings = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if !matches!(item, Json::Object(_)) {
            return Err(format!("finding {index} is not an object"));
        }
        let id = item
            .get("id")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("finding {index} has no string id"))?
            .to_string();
        let severity_text = item
            .get("severity")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("finding {index} has no string severity"))?;
        let severity = match severity_text {
            "ok" => Severity::Ok,
            "warn" => Severity::Warn,
            "bad" => Severity::Bad,
            other => return Err(format!("finding {index} has unknown severity {other:?}")),
        };
        let title = item
            .get("title")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("finding {index} has no string title"))?
            .to_string();
        let detail = item
            .get("detail")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("finding {index} has no string detail"))?
            .to_string();
        let fix_available = item
            .get("fix_available")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        findings.push(Finding {
            id,
            severity,
            title,
            detail,
            fix_available,
        });
    }
    Ok(findings)
}

fn current_exe_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn find_script() -> Option<PathBuf> {
    if let Ok(path) = env::var("HDD_WATCH_DOCTOR") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(dir) = current_exe_dir() {
        let candidate = dir.join("fedora-doctor.sh");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let cwd = env::current_dir().ok();
    if let Some(cwd) = cwd {
        let candidate = cwd.join("fedora-doctor.sh");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    find_in_path("fedora-doctor.sh")
}

fn script_path() -> Result<PathBuf, String> {
    find_script().ok_or_else(|| {
        "fedora-doctor.sh not found (looked next to the binary, in the current directory and in PATH; set HDD_WATCH_DOCTOR)".to_string()
    })
}

pub fn run_json(script: Option<&PathBuf>) -> Result<Vec<Finding>, String> {
    let script = match script {
        Some(path) => path.clone(),
        None => script_path()?,
    };
    let output = Command::new(&script)
        .arg("--json")
        .output()
        .map_err(|err| format!("cannot run {}: {err}", script.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{} --json failed: {}",
            script.display(),
            stderr.trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_findings(&text)
}

pub fn run_fix_preview(id: &str) -> Result<String, String> {
    let script = script_path()?;
    let output = Command::new(&script)
        .arg("--fix-id")
        .arg(id)
        .arg("--dry-run")
        .output()
        .map_err(|err| format!("cannot run {}: {err}", script.display()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FixEvent {
    Line(String),
    Done(Result<(), String>),
}

pub fn run_fix_streaming(id: &str, sender: Sender<FixEvent>) {
    let script = match script_path() {
        Ok(script) => script,
        Err(err) => {
            let _ = sender.send(FixEvent::Done(Err(err)));
            return;
        }
    };
    let mut child = match Command::new(&script)
        .arg("--fix-id")
        .arg(id)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let _ = sender.send(FixEvent::Done(Err(format!("cannot run fix: {err}"))));
            return;
        }
    };
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let tx = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(FixEvent::Line(line));
            }
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(FixEvent::Line(line));
            }
        }));
    }
    for reader in readers {
        let _ = reader.join();
    }
    match child.wait() {
        Ok(status) if status.success() => {
            let _ = sender.send(FixEvent::Done(Ok(())));
        }
        Ok(status) => {
            let _ = sender.send(FixEvent::Done(Err(format!("fix exited with {status}"))));
        }
        Err(err) => {
            let _ = sender.send(FixEvent::Done(Err(format!("cannot wait for fix: {err}"))));
        }
    }
}

pub fn run_fix_all_streaming(sender: Sender<FixEvent>) {
    let script = match script_path() {
        Ok(script) => script,
        Err(err) => {
            let _ = sender.send(FixEvent::Done(Err(err)));
            return;
        }
    };
    let mut child = match Command::new(&script)
        .arg("--fix")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let _ = sender.send(FixEvent::Done(Err(format!("cannot run fix: {err}"))));
            return;
        }
    };
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let tx = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(FixEvent::Line(line));
            }
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = sender.clone();
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(FixEvent::Line(line));
            }
        }));
    }
    for reader in readers {
        let _ = reader.join();
    }
    match child.wait() {
        Ok(status) if status.success() => {
            let _ = sender.send(FixEvent::Done(Ok(())));
        }
        Ok(status) => {
            let _ = sender.send(FixEvent::Done(Err(format!("fix exited with {status}"))));
        }
        Err(err) => {
            let _ = sender.send(FixEvent::Done(Err(format!("cannot wait for fix: {err}"))));
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SmartInfo {
    pub device: String,
    pub model: String,
    pub health: String,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskInfo {
    pub smart: Vec<SmartInfo>,
    pub filesystem: String,
    pub usage: String,
    pub mounts: String,
    pub schedulers: Vec<(String, String)>,
}

fn capture(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if err.is_empty() {
            None
        } else {
            Some(err)
        }
    } else {
        Some(text)
    }
}

fn smart_attribute(text: &str, id: &str) -> Option<u64> {
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() == Some(&id) {
            return fields.last().and_then(|value| value.parse::<u64>().ok());
        }
    }
    None
}

fn disk_names() -> Vec<String> {
    let Some(output) = capture("lsblk", &["-dno", "NAME,ROTA,TYPE"]) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 || fields[2] != "disk" {
                return None;
            }
            let name = fields[0];
            if name.starts_with("loop") || name.starts_with("zram") {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

pub fn collect_disk_info() -> DiskInfo {
    let mut info = DiskInfo::default();
    for name in disk_names() {
        let device = format!("/dev/{name}");
        let health_text = capture("smartctl", &["-H", &device]).unwrap_or_default();
        let attr_text = capture("smartctl", &["-A", &device]).unwrap_or_default();
        let model_text = capture("smartctl", &["-i", &device]).unwrap_or_default();
        let health = health_text
            .lines()
            .find(|line| line.contains("overall-health"))
            .and_then(|line| line.split(':').next_back())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| "unknown (smartctl unavailable or no permission)".to_string());
        let model = model_text
            .lines()
            .find(|line| line.starts_with("Device Model:") || line.starts_with("Model Number:"))
            .and_then(|line| line.split(':').next_back())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| name.clone());
        info.smart.push(SmartInfo {
            device: device.clone(),
            model,
            health,
            reallocated: smart_attribute(&attr_text, "5"),
            pending: smart_attribute(&attr_text, "197"),
        });
        let scheduler_path = format!("/sys/block/{name}/queue/scheduler");
        if let Ok(scheduler) = fs::read_to_string(scheduler_path) {
            info.schedulers
                .push((name.clone(), scheduler.trim().to_string()));
        }
    }
    let mount = capture("findmnt", &["-no", "FSTYPE,OPTIONS", "/"]).unwrap_or_default();
    if let Some((fs, opts)) = mount.split_once(' ') {
        info.filesystem = fs.to_string();
        info.mounts = opts.to_string();
    } else {
        info.filesystem = mount;
    }
    info.usage = capture("df", &["-hP", "/"])
        .and_then(|text| text.lines().nth(1).map(str::to_string))
        .unwrap_or_default();
    info
}

pub fn disk_advice(findings: &[Finding]) -> Vec<String> {
    let ids = [
        "disk-health",
        "filesystem-usage",
        "filesystem-btrfs-space",
        "filesystem-ext4-frag",
        "filesystem-mountopts",
        "io-scheduler",
    ];
    findings
        .iter()
        .filter(|finding| ids.contains(&finding.id.as_str()))
        .filter(|finding| finding.severity != Severity::Ok)
        .map(|finding| format!("{}: {}", finding.title, finding.detail))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_doctor_json() {
        let text = r#"[
          {"id":"disk-health","severity":"warn","title":"Disk","detail":"quote \" and newline\nand backslash \\","fix_available":false},
          {"id":"io-scheduler","severity":"bad","title":"Scheduler","detail":"bfq","fix_available":true},
          {"id":"startup-dns","severity":"ok","title":"DNS","detail":"fast","fix_available":false}
        ]"#;
        let findings = parse_findings(text).expect("parse");
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].detail.contains("quote \" and newline\n"));
        assert!(findings[1].fix_available);
        assert_eq!(findings[2].severity, Severity::Ok);
    }

    #[test]
    fn parses_unicode_and_escapes() {
        let text = r#"[{"id":"x","severity":"ok","title":"caf\u00e9","detail":"tab\there","fix_available":false}]"#;
        let findings = parse_findings(text).expect("parse");
        assert_eq!(findings[0].title, "café");
        assert_eq!(findings[0].detail, "tab\there");
    }

    #[test]
    fn combines_unicode_surrogate_pairs_safely() {
        let text = r#"[{"id":"x","severity":"ok","title":"\ud83d\ude00","detail":"d","fix_available":false}]"#;
        let findings = parse_findings(text).expect("parse");
        assert_eq!(findings[0].title, "😀");
        let bad_low = r#"[{"id":"x","severity":"ok","title":"\ud83d\u0041","detail":"d","fix_available":false}]"#;
        assert!(parse_findings(bad_low).is_err());
        let lone_high =
            r#"[{"id":"x","severity":"ok","title":"\ud83d","detail":"d","fix_available":false}]"#;
        assert!(parse_findings(lone_high).is_err());
    }

    #[test]
    fn rejects_invalid_or_wrong_shapes() {
        assert!(parse_findings("").is_err());
        assert!(parse_findings("{}").is_err());
        assert!(parse_findings("[1]").is_err());
        assert!(
            parse_findings(r#"[{"id":"x","severity":"meh","title":"t","detail":"d"}]"#).is_err()
        );
        assert!(parse_findings(r#"[{"id":"x","severity":"ok","title":"t"}]"#).is_err());
        assert!(parse_findings(
            r#"[{"id":"x","severity":"ok","title":"t","detail":"d"}] trailing"#
        )
        .is_err());
    }

    #[test]
    fn missing_fix_available_defaults_false() {
        let findings = parse_findings(r#"[{"id":"x","severity":"ok","title":"t","detail":"d"}]"#)
            .expect("parse");
        assert!(!findings[0].fix_available);
    }

    #[test]
    fn smart_attribute_reads_last_column() {
        let text = "  5 Reallocated_Sector_Ct   0x0033   100   100   010    Pre-fail  Always       -       4\n197 Current_Pending_Sector  0x0012   100   100   000    Old_age   Always       -       2\n";
        assert_eq!(smart_attribute(text, "5"), Some(4));
        assert_eq!(smart_attribute(text, "197"), Some(2));
    }
}
