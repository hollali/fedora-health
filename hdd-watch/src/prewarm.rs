use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::apps;
use crate::logfmt;
use crate::sampler;

#[derive(Clone, Debug)]
pub struct PrewarmEntry {
    pub label: String,
    pub command: String,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PrewarmPlan {
    pub entries: Vec<PrewarmEntry>,
    pub files: Vec<PathBuf>,
    pub total_bytes: u64,
    pub mem_available_bytes: u64,
}

pub fn parse_config_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

pub fn check_ram_margin(total_bytes: u64, available_bytes: u64) -> Result<(), String> {
    if available_bytes == 0 {
        return Err("cannot read MemAvailable; refusing to prewarm".to_string());
    }
    let limit = available_bytes / 4;
    if total_bytes > limit {
        return Err(format!(
            "refusing to prewarm: {:.1} MB of files is more than 25% of MemAvailable ({:.1} MB, limit {:.1} MB).\nWarm fewer apps or free memory first.",
            total_bytes as f64 / 1024.0 / 1024.0,
            available_bytes as f64 / 1024.0 / 1024.0,
            limit as f64 / 1024.0 / 1024.0
        ));
    }
    Ok(())
}

pub fn parse_ldd(output: &str) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    for line in output.lines() {
        let path = if let Some((_, after)) = line.split_once("=>") {
            after.split_whitespace().next()
        } else {
            line.split_whitespace().next()
        };
        let Some(path) = path else { continue };
        let path = PathBuf::from(path);
        if path.is_absolute() && path.exists() && !path.to_string_lossy().contains("vdso") {
            libs.push(path);
        }
    }
    libs
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn resolve_binary(argv: &[String]) -> Option<PathBuf> {
    let mut iter = argv.iter().peekable();
    if iter.peek().is_some_and(|arg| arg.as_str() == "env") {
        iter.next();
        while iter.peek().is_some_and(|arg| arg.contains('=')) {
            iter.next();
        }
    }
    let binary = iter.next()?;
    let path = if binary.contains('/') {
        PathBuf::from(binary)
    } else {
        find_in_path(binary)?
    };
    if path.is_file() {
        Some(fs::canonicalize(&path).unwrap_or(path))
    } else {
        None
    }
}

fn trace_files(app: &str) -> Vec<PathBuf> {
    let path = logfmt::state_dir()
        .join("traces")
        .join(format!("{}.files", logfmt::sanitize_name(app)));
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .collect()
}

fn resolve_entry(line: &str) -> Result<(String, String, Vec<PathBuf>), String> {
    let (label, command, argv) = if line.ends_with(".desktop") {
        let entry =
            apps::find_desktop(line).ok_or_else(|| format!("desktop entry not found: {line}"))?;
        let argv = apps::parse_exec_argv(&entry.exec);
        (entry.name, entry.exec, argv)
    } else {
        let argv = apps::parse_exec_argv(line);
        let label = argv
            .first()
            .map(|arg| {
                Path::new(arg)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(arg)
                    .to_string()
            })
            .unwrap_or_else(|| line.to_string());
        (label, line.to_string(), argv)
    };
    if argv.is_empty() {
        return Err(format!("no command in: {line}"));
    }
    let binary =
        resolve_binary(&argv).ok_or_else(|| format!("cannot resolve binary for: {line}"))?;
    let mut files = vec![binary.clone()];
    if let Ok(output) = Command::new("ldd")
        .arg(&binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        files.extend(parse_ldd(&String::from_utf8_lossy(&output.stdout)));
    }
    files.extend(trace_files(&label));
    Ok((label, command, files))
}

pub fn resolve_plan(config_path: &Path) -> Result<PrewarmPlan, String> {
    let content = fs::read_to_string(config_path).map_err(|err| {
        format!(
            "cannot read {}: {err}\nInstall a config with the package or start from prewarm.conf.example.",
            config_path.display()
        )
    })?;
    let mem = sampler::read_meminfo();
    let mut plan = PrewarmPlan {
        mem_available_bytes: mem.available_kb.saturating_mul(1024),
        ..PrewarmPlan::default()
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for line in parse_config_lines(&content) {
        let (label, command, files) = resolve_entry(&line)?;
        let mut entry_files = Vec::new();
        let mut bytes = 0u64;
        for file in files {
            if !seen.insert(file.clone()) {
                continue;
            }
            let size = fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
            bytes = bytes.saturating_add(size);
            plan.total_bytes = plan.total_bytes.saturating_add(size);
            plan.files.push(file.clone());
            entry_files.push(file);
        }
        plan.entries.push(PrewarmEntry {
            label,
            command,
            files: entry_files,
            bytes,
        });
    }
    Ok(plan)
}

pub fn warm_files(files: &[PathBuf]) -> (usize, u64, usize) {
    let mut buffer = vec![0u8; 256 * 1024];
    let mut warmed = 0usize;
    let mut bytes = 0u64;
    let mut failed = 0usize;
    for file in files {
        let Ok(mut handle) = File::open(file) else {
            failed = failed.saturating_add(1);
            continue;
        };
        loop {
            match handle.read(&mut buffer) {
                Ok(0) => {
                    warmed = warmed.saturating_add(1);
                    break;
                }
                Ok(read) => {
                    bytes = bytes.saturating_add(read as u64);
                }
                Err(_) => {
                    failed = failed.saturating_add(1);
                    break;
                }
            }
        }
    }
    (warmed, bytes, failed)
}

fn command_exists(name: &str) -> bool {
    find_in_path(name).is_some()
}

fn spawn_worker(manifest: &Path) -> Result<(), String> {
    let exe = env::current_exe().map_err(|err| format!("cannot find own binary: {err}"))?;
    let mut cmd;
    if command_exists("ionice") && command_exists("nice") {
        cmd = Command::new("ionice");
        cmd.args(["-c3", "nice", "-n", "19"]).arg(&exe);
    } else if command_exists("nice") {
        cmd = Command::new("nice");
        cmd.arg("-n").arg("19").arg(&exe);
        eprintln!("hdd-watch: ionice not found; using nice only");
    } else {
        cmd = Command::new(&exe);
        eprintln!("hdd-watch: ionice and nice not found; warming at normal priority");
    }
    let status = cmd
        .args(["prewarm", "--worker"])
        .arg(manifest)
        .env("HDD_WATCH_PREWARM_WORKER", "1")
        .status()
        .map_err(|err| format!("cannot start prewarm worker: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("prewarm worker exited with {status}"))
    }
}

pub fn run_worker(manifest: &Path) -> Result<(), String> {
    let content = fs::read_to_string(manifest)
        .map_err(|err| format!("cannot read manifest {}: {err}", manifest.display()))?;
    let files: Vec<PathBuf> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    let total: u64 = files
        .iter()
        .map(|file| fs::metadata(file).map(|meta| meta.len()).unwrap_or(0))
        .fold(0u64, |acc, size| acc.saturating_add(size));
    let mem = sampler::read_meminfo();
    check_ram_margin(total, mem.available_kb.saturating_mul(1024))?;
    println!(
        "warming {} files ({:.1} MB) at idle I/O priority...",
        files.len(),
        total as f64 / 1024.0 / 1024.0
    );
    let (warmed, bytes, failed) = warm_files(&files);
    println!(
        "warmed {warmed} files ({:.1} MB), {failed} could not be read",
        bytes as f64 / 1024.0 / 1024.0
    );
    Ok(())
}

pub fn run(suggest: bool) -> Result<(), String> {
    if suggest {
        println!("{}", render_suggestions());
        return Ok(());
    }
    let config = logfmt::prewarm_conf_path();
    let plan = resolve_plan(&config)?;
    if plan.entries.is_empty() {
        return Err(format!(
            "no apps configured in {}\nAdd one command or .desktop id per line.",
            config.display()
        ));
    }
    for entry in &plan.entries {
        println!(
            "  {:<28} {:>4} files {:>8.1} MB  {}",
            entry.label,
            entry.files.len(),
            entry.bytes as f64 / 1024.0 / 1024.0,
            entry.command
        );
    }
    println!(
        "total: {} files, {:.1} MB; MemAvailable {:.1} MB (25% limit {:.1} MB)",
        plan.files.len(),
        plan.total_bytes as f64 / 1024.0 / 1024.0,
        plan.mem_available_bytes as f64 / 1024.0 / 1024.0,
        plan.mem_available_bytes as f64 / 1024.0 / 1024.0 / 4.0
    );
    check_ram_margin(plan.total_bytes, plan.mem_available_bytes)?;
    let dir = logfmt::state_dir();
    fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let manifest = dir.join(format!("prewarm-manifest-{}.txt", std::process::id()));
    let mut content = String::new();
    for file in &plan.files {
        content.push_str(&file.display().to_string());
        content.push('\n');
    }
    fs::write(&manifest, content)
        .map_err(|err| format!("cannot write {}: {err}", manifest.display()))?;
    let result = spawn_worker(&manifest);
    let _ = fs::remove_file(&manifest);
    result
}

pub fn render_suggestions() -> String {
    let history = crate::launch::load_history();
    if history.is_empty() {
        return format!(
            "No launch history in {}; benchmark apps with --cold first.",
            logfmt::launches_path().display()
        );
    }
    let mut by_app: std::collections::HashMap<String, (Vec<u64>, Vec<u64>, String)> =
        std::collections::HashMap::new();
    for record in history {
        let entry = by_app
            .entry(record.app.clone())
            .or_insert_with(|| (Vec::new(), Vec::new(), record.command.clone()));
        if record.kind == "cold" {
            entry.0.push(record.settled_ms);
        } else {
            entry.1.push(record.settled_ms);
        }
    }
    let mut suggestions = Vec::new();
    for (app, (cold, warm, command)) in by_app {
        if cold.is_empty() || warm.is_empty() {
            continue;
        }
        let cold_avg = cold.iter().sum::<u64>() as f64 / cold.len() as f64;
        let warm_avg = warm.iter().sum::<u64>() as f64 / warm.len() as f64;
        if warm_avg <= 0.0 {
            continue;
        }
        let ratio = cold_avg / warm_avg;
        if ratio > 2.0 && cold_avg > 3000.0 {
            suggestions.push((app, command, cold_avg, warm_avg, ratio));
        }
    }
    suggestions.sort_by(|a, b| b.4.total_cmp(&a.4));
    if suggestions.is_empty() {
        return "No apps currently qualify (need cold start > 3 s and cold/warm ratio > 2).\n"
            .to_string();
    }
    let mut out = String::from("Suggested prewarm.conf entries:\n\n");
    for (app, command, cold, warm, ratio) in suggestions {
        out.push_str(&format!(
            "# {app}: cold {:.2}s / warm {:.2}s = {ratio:.2}x\n{command}\n\n",
            cold / 1000.0,
            warm / 1000.0
        ));
    }
    out
}

pub fn add_to_config(line: &str) -> Result<String, String> {
    let line = line.trim();
    if line.is_empty() || line.contains('\n') || line.contains('\r') {
        return Err("invalid prewarm config line".to_string());
    }
    let path = logfmt::prewarm_conf_path();
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if parse_config_lines(&existing)
        .iter()
        .any(|entry| entry == line)
    {
        return Ok(format!("already present in {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(line);
    content.push('\n');
    fs::write(&path, content).map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(format!("added to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_margin_refuses_over_quarter() {
        let available = 4_000 * 1024 * 1024;
        assert!(check_ram_margin(999 * 1024 * 1024, available).is_ok());
        assert!(check_ram_margin(1001 * 1024 * 1024, available).is_err());
        assert!(check_ram_margin(0, available).is_ok());
        assert!(check_ram_margin(0, 0).is_err());
    }

    #[test]
    fn config_parser_skips_comments_and_blanks() {
        let lines = parse_config_lines("# comment\n\n  firefox  \n/usr/bin/code\n");
        assert_eq!(lines, vec!["firefox", "/usr/bin/code"]);
    }

    #[test]
    fn ldd_parser_extracts_absolute_existing_libraries() {
        let output = "\tlinux-vdso.so.1 (0x00007fff)\n\tlibc.so.6 => /lib64/libc.so.6 (0x00007f)\n\t/lib64/ld-linux-x86-64.so.2 (0x00007f)\n\tmissing => not found\n";
        let libs = parse_ldd(output);
        assert!(libs.contains(&PathBuf::from("/lib64/libc.so.6")));
        assert!(libs.contains(&PathBuf::from("/lib64/ld-linux-x86-64.so.2")));
        assert!(!libs
            .iter()
            .any(|path| path.to_string_lossy().contains("vdso")));
    }
}
