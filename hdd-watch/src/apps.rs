use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::logfmt;
use crate::sampler;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub no_display: bool,
    pub hidden: bool,
    pub source: PathBuf,
}

pub fn is_field_code(ch: char) -> bool {
    matches!(
        ch,
        'f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm'
    )
}

pub fn parse_exec_argv(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = exec.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if ch == '%' {
                match chars.next() {
                    Some('%') => current.push('%'),
                    Some(code) if is_field_code(code) => {}
                    Some(code) => {
                        current.push('%');
                        current.push(code);
                    }
                    None => current.push('%'),
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            '%' => match chars.next() {
                Some('%') => current.push('%'),
                Some(code) if is_field_code(code) => {}
                Some(code) => {
                    current.push('%');
                    current.push(code);
                }
                None => current.push('%'),
            },
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub fn strip_exec_field_codes(exec: &str) -> String {
    parse_exec_argv(exec).join(" ")
}

pub fn parse_desktop(content: &str, id: &str, source: &Path) -> Option<DesktopEntry> {
    let mut in_main = false;
    let mut name = String::new();
    let mut exec = String::new();
    let mut no_display = false;
    let mut hidden = false;
    let mut kind = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => name = value.trim().to_string(),
            "Exec" => exec = value.trim().to_string(),
            "NoDisplay" => no_display = value.trim().eq_ignore_ascii_case("true"),
            "Hidden" => hidden = value.trim().eq_ignore_ascii_case("true"),
            "Type" => kind = value.trim().to_string(),
            _ => {}
        }
    }
    if exec.is_empty() {
        return None;
    }
    if !kind.is_empty() && kind != "Application" {
        return None;
    }
    if name.is_empty() {
        name = id.trim_end_matches(".desktop").to_string();
    }
    Some(DesktopEntry {
        id: id.to_string(),
        name,
        exec,
        no_display,
        hidden,
        source: source.to_path_buf(),
    })
}

pub fn read_desktop_file(path: &Path) -> Option<DesktopEntry> {
    let content = fs::read_to_string(path).ok()?;
    let id = path.file_name()?.to_string_lossy().to_string();
    parse_desktop(&content, &id, path)
}

pub fn desktop_dirs() -> Vec<PathBuf> {
    let user = logfmt::real_user();
    vec![
        user.home.join(".local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        user.home
            .join(".local/share/flatpak/exports/share/applications"),
    ]
}

pub fn apps_in_dir(dir: &Path) -> Vec<DesktopEntry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
        .filter_map(|path| read_desktop_file(&path))
        .collect()
}

pub fn installed_apps() -> Vec<DesktopEntry> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in desktop_dirs() {
        for app in apps_in_dir(&dir) {
            if app.no_display || app.hidden {
                continue;
            }
            if parse_exec_argv(&app.exec).is_empty() {
                continue;
            }
            if seen.insert(app.id.clone()) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

pub fn find_desktop(id: &str) -> Option<DesktopEntry> {
    for dir in desktop_dirs() {
        let path = dir.join(id);
        if path.is_file() {
            if let Some(entry) = read_desktop_file(&path) {
                return Some(entry);
            }
        }
    }
    None
}

pub fn autostart_dirs() -> Vec<PathBuf> {
    let user = logfmt::real_user();
    vec![
        user.home.join(".config/autostart"),
        PathBuf::from("/etc/xdg/autostart"),
    ]
}

pub fn autostart_entries() -> Vec<DesktopEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for dir in autostart_dirs() {
        for entry in apps_in_dir(&dir) {
            if seen.insert(entry.id.clone()) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|a| a.name.to_lowercase());
    entries
}

fn user_command(program: &str, args: &[&str]) -> Command {
    let user = logfmt::real_user();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", user.uid))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{}/bus", user.uid),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run_user(program: &str, args: &[&str]) -> String {
    match user_command(program, args).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(output) => {
            let err = String::from_utf8_lossy(&output.stderr);
            if err.trim().is_empty() {
                String::new()
            } else {
                format!("(command failed: {})", err.trim())
            }
        }
        Err(err) => format!("(cannot run {program}: {err})"),
    }
}

fn boot_time() -> u64 {
    fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|content| {
            content.lines().find_map(|line| {
                line.strip_prefix("btime ")
                    .and_then(|value| value.trim().parse::<u64>().ok())
            })
        })
        .unwrap_or(0)
}

fn cmdline(pid: u32) -> String {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| {
            bytes
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct SessionProc {
    pub pid: u32,
    pub name: String,
    pub read_bytes: u64,
}

pub fn session_processes(uid: u32, window_secs: u64) -> (Vec<SessionProc>, u64) {
    let procs = sampler::read_all_procs();
    let mut session_start: Option<u64> = None;
    for proc in &procs {
        if proc.name.starts_with("systemd") && cmdline(proc.pid).contains("--user") {
            session_start = Some(match session_start {
                Some(current) => current.min(proc.starttime),
                None => proc.starttime,
            });
        }
    }
    let owned: Vec<&sampler::ProcStat> = procs
        .iter()
        .filter(|proc| {
            fs::metadata(format!("/proc/{}", proc.pid))
                .map(|meta| meta.uid() == uid)
                .unwrap_or(false)
        })
        .collect();
    let session_start =
        session_start.unwrap_or_else(|| owned.iter().map(|proc| proc.starttime).min().unwrap_or(0));
    let window_ticks = window_secs.saturating_mul(100);
    let mut out = Vec::new();
    let mut total = 0u64;
    for proc in owned {
        if proc.starttime < session_start
            || proc.starttime > session_start.saturating_add(window_ticks)
        {
            continue;
        }
        let read_bytes = sampler::read_proc_io(proc.pid)
            .map(|io| io.read_bytes)
            .unwrap_or(0);
        total = total.saturating_add(read_bytes);
        out.push(SessionProc {
            pid: proc.pid,
            name: proc.name.clone(),
            read_bytes,
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.read_bytes));
    (out, total)
}

pub fn render_audit() -> String {
    let user = logfmt::real_user();
    let entries = autostart_entries();
    let (session, total_bytes) = session_processes(user.uid, 120);
    let mut out = String::new();
    out.push_str(&format!(
        "startup audit for {} (uid {}) — session start {}\n",
        user.name,
        user.uid,
        if boot_time() > 0 {
            "detected"
        } else {
            "unknown"
        }
    ));
    out.push_str(&format!(
        "\nautostart entries: {} {}\n",
        entries.len(),
        if entries.len() > 10 {
            "(WARN: more than 10 — this delays login)"
        } else {
            "(ok)"
        }
    ));
    let by_name: HashMap<String, u64> = session
        .iter()
        .map(|proc| (proc.name.clone(), proc.read_bytes))
        .collect();
    for entry in &entries {
        let argv = parse_exec_argv(&entry.exec);
        let bin = argv
            .iter()
            .find(|arg| !arg.contains('='))
            .map(|arg| {
                Path::new(arg)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(arg)
                    .to_string()
            })
            .unwrap_or_else(|| "?".to_string());
        let read = by_name.get(&bin).copied().unwrap_or(0);
        out.push_str(&format!(
            "  {:<32} {:<8} {:>8.1} MB  {}\n",
            truncate(&entry.name, 32),
            if entry.hidden { "hidden" } else { "enabled" },
            read as f64 / 1024.0 / 1024.0,
            entry.exec
        ));
    }
    if entries.len() > 10 {
        out.push_str("  advice: disable the heaviest entries with your desktop's Startup Applications tool.\n");
    }

    out.push_str("\nenabled systemctl --user services:\n");
    let services = run_user(
        "systemctl",
        &[
            "--user",
            "list-unit-files",
            "--state=enabled",
            "--no-legend",
            "--no-pager",
        ],
    );
    if services.is_empty() {
        out.push_str("  (none or user bus unavailable)\n");
    } else {
        for line in services.lines().take(40) {
            out.push_str(&format!("  {line}\n"));
        }
    }

    out.push_str("\nsystemd-analyze --user blame (top 10):\n");
    let blame = run_user("systemd-analyze", &["--user", "blame"]);
    if blame.is_empty() {
        out.push_str("  (unavailable)\n");
    } else {
        for line in blame.lines().take(10) {
            out.push_str(&format!("  {line}\n"));
        }
    }

    out.push_str("\nsystemd-analyze --user critical-chain:\n");
    let chain = run_user("systemd-analyze", &["--user", "critical-chain"]);
    if chain.is_empty() {
        out.push_str("  (unavailable)\n");
    } else {
        for line in chain.lines().take(30) {
            out.push_str(&format!("  {line}\n"));
        }
    }

    out.push_str(&format!(
        "\nprocesses started in the first 2 minutes, by disk read (total {:.1} MB):\n",
        total_bytes as f64 / 1024.0 / 1024.0
    ));
    for proc in session.iter().take(25) {
        out.push_str(&format!(
            "  {:>8.1} MB  {:<20} pid {}\n",
            proc.read_bytes as f64 / 1024.0 / 1024.0,
            truncate(&proc.name, 20),
            proc.pid
        ));
    }

    let mut offenders: Vec<&SessionProc> = session.iter().take(5).collect();
    offenders.sort_by_key(|a| std::cmp::Reverse(a.read_bytes));
    if !offenders.is_empty() {
        out.push_str("\ntop offenders to review (never disabled automatically):\n");
        for (idx, proc) in offenders.iter().enumerate() {
            let matched = entries.iter().find(|entry| {
                parse_exec_argv(&entry.exec).iter().any(|arg| {
                    Path::new(arg)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == proc.name)
                })
            });
            out.push_str(&format!(
                "  {}. {:<20} {:>8.1} MB read{}{}\n",
                idx + 1,
                proc.name,
                proc.read_bytes as f64 / 1024.0 / 1024.0,
                if let Some(entry) = matched {
                    format!(" — autostart: {}", entry.name)
                } else {
                    String::new()
                },
                if matched.is_some() {
                    " (consider disabling)"
                } else {
                    ""
                }
            ));
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

    #[test]
    fn exec_field_codes_stripped() {
        assert_eq!(
            strip_exec_field_codes("/usr/bin/firefox %u"),
            "/usr/bin/firefox"
        );
        assert_eq!(
            strip_exec_field_codes("/usr/bin/code --new-window %F --user-data-dir=%U"),
            "/usr/bin/code --new-window --user-data-dir="
        );
        assert_eq!(
            parse_exec_argv("/usr/bin/app \"file with spaces\" %f"),
            vec!["/usr/bin/app".to_string(), "file with spaces".to_string()]
        );
        assert_eq!(
            strip_exec_field_codes("/usr/bin/percent %%"),
            "/usr/bin/percent %"
        );
    }

    #[test]
    fn desktop_parser_reads_main_group() {
        let content = "[Desktop Entry]\nType=Application\nName=Test App\nExec=test %U\nNoDisplay=true\nHidden=true\n\n[Desktop Action foo]\nName=Other\nExec=other\n";
        let entry =
            parse_desktop(content, "test.desktop", Path::new("/tmp/test.desktop")).expect("entry");
        assert_eq!(entry.name, "Test App");
        assert_eq!(entry.exec, "test %U");
        assert!(entry.no_display);
        assert!(entry.hidden);
    }

    #[test]
    fn desktop_parser_rejects_non_apps() {
        let content = "[Desktop Entry]\nType=Link\nName=Link\nExec=x\n";
        assert!(parse_desktop(content, "x.desktop", Path::new("/tmp/x")).is_none());
        assert!(parse_desktop("[Desktop Entry]\nName=x\n", "x", Path::new("/tmp/x")).is_none());
    }
}
