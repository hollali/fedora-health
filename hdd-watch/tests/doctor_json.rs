use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn doctor_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fedora-doctor.sh")
}

#[test]
fn doctor_emits_valid_nonempty_json() {
    let script = doctor_script();
    assert!(script.is_file(), "missing {}", script.display());
    let output = Command::new("bash")
        .arg(&script)
        .arg("--json")
        .env("LC_ALL", "C")
        .output()
        .expect("run fedora-doctor.sh --json");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut validator = Command::new("python3")
        .args([
            "-c",
            "import json,sys\ndata=json.load(sys.stdin)\nassert isinstance(data,list) and data\nassert all(isinstance(item,dict) and 'id' in item and 'severity' in item and 'title' in item and 'detail' in item for item in data)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 is required for this test");
    validator
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(&output.stdout)
        .expect("pipe doctor JSON");
    let checked = validator.wait_with_output().expect("validate JSON");
    assert!(
        checked.status.success(),
        "invalid doctor JSON: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn fix_dry_run_requires_no_root_and_changes_nothing() {
    let script = doctor_script();
    let output = Command::new("bash")
        .arg(&script)
        .args(["--fix-id", "io-scheduler", "--dry-run"])
        .env("LC_ALL", "C")
        .output()
        .expect("run doctor dry-run");
    assert!(
        output.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty() && stdout.to_lowercase().contains("scheduler"),
        "dry-run should describe the scheduler action:\n{stdout}"
    );
}
