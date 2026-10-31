use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempState {
    path: PathBuf,
}

impl TempState {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "hdd-watch-test-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp state dir");
        Self { path }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hdd-watch"));
        command
            .env("HDD_WATCH_STATE", &self.path)
            .env("HDD_WATCH_LOG", self.path.join("samples.log"))
            .env("HDD_WATCH_LAUNCHES", self.path.join("launches.log"))
            .env("HDD_WATCH_PREWARM_CONF", self.path.join("prewarm.conf"));
        command
    }
}

impl Drop for TempState {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn launches_log_and_reports_sleep() {
    let state = TempState::new("sleep");
    let output = state
        .command()
        .args(["launch", "--timeout", "10", "--", "sleep", "2"])
        .output()
        .expect("run hdd-watch launch");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("sleep [warm]"), "stdout:\n{stdout}");
    assert!(stdout.contains("verdict:"), "stdout:\n{stdout}");

    let launches =
        fs::read_to_string(state.path.join("launches.log")).expect("launch log should be written");
    assert!(launches.contains("|sleep|warm|"), "launches:\n{launches}");
}

#[test]
fn launches_disk_reader_without_crashing() {
    let state = TempState::new("reader");
    let output = state
        .command()
        .args([
            "launch",
            "--timeout",
            "15",
            "--",
            "sh",
            "-c",
            "cat /usr/bin/* > /dev/null",
        ])
        .output()
        .expect("run hdd-watch launch");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("sh [warm]"), "stdout:\n{stdout}");
    assert!(stdout.contains("verdict:"), "stdout:\n{stdout}");

    let launches =
        fs::read_to_string(state.path.join("launches.log")).expect("launch log should be written");
    assert!(launches.contains("|sh|warm|"), "launches:\n{launches}");
}
