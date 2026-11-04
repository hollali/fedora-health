use std::collections::{HashMap, HashSet};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceInfo {
    pub unit: String,
    pub boot_time: String,
    pub boot_secs: u64,
    pub state: String,
    pub active: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Enable,
    Disable,
    Mask,
    Unmask,
}

impl ServiceAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Mask => "mask",
            Self::Unmask => "unmask",
        }
    }

    pub fn effect(self) -> &'static str {
        match self {
            Self::Enable => "enabled at the next boot",
            Self::Disable => "disabled and will not start at the next boot",
            Self::Mask => "masked and cannot start until it is unmasked",
            Self::Unmask => "unmasked and allowed to start again",
        }
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!("{program} {} failed", args.join(" ")));
        }
        return Err(stderr);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn collect_services() -> Result<Vec<ServiceInfo>, String> {
    let unit_files = run(
        "systemctl",
        &[
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
        ],
    )?;
    let blame = run("systemd-analyze", &["blame", "--no-pager"]).unwrap_or_default();
    let units = run(
        "systemctl",
        &[
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--no-pager",
        ],
    )
    .unwrap_or_default();

    let blame_map = parse_blame(&blame);
    let active_map = parse_active(&units);
    let mut seen = HashSet::new();
    let mut services = Vec::new();

    for (unit, state) in parse_unit_files(&unit_files) {
        seen.insert(unit.clone());
        let (boot_time, boot_secs) = blame_map.get(&unit).cloned().unwrap_or_default();
        services.push(ServiceInfo {
            active: active_map.get(&unit).cloned().unwrap_or_default(),
            unit,
            boot_time,
            boot_secs,
            state,
        });
    }

    for (unit, (boot_time, boot_secs)) in blame_map {
        if !seen.insert(unit.clone()) {
            continue;
        }
        services.push(ServiceInfo {
            active: active_map.get(&unit).cloned().unwrap_or_default(),
            unit,
            boot_time,
            boot_secs,
            state: "unknown".to_string(),
        });
    }

    services.sort_by(|a, b| {
        b.boot_secs
            .cmp(&a.boot_secs)
            .then_with(|| a.unit.cmp(&b.unit))
    });
    Ok(services)
}

pub fn apply_action(unit: &str, action: ServiceAction) -> Result<String, String> {
    if !unit.ends_with(".service") || unit.contains('/') {
        return Err(format!("invalid service unit: {unit}"));
    }
    let output = Command::new("systemctl")
        .arg(action.verb())
        .arg(unit)
        .output()
        .map_err(|err| format!("cannot run systemctl: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let message = [stdout, stderr]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if message.is_empty() {
            Ok(format!("{} {}", action.verb(), unit))
        } else {
            Ok(message)
        }
    } else if stderr.is_empty() {
        Err(format!("systemctl {} {} failed", action.verb(), unit))
    } else {
        Err(stderr)
    }
}

fn parse_unit_files(text: &str) -> Vec<(String, String)> {
    let mut services = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(unit) = parts.next() else {
            continue;
        };
        if !unit.ends_with(".service") {
            continue;
        }
        let state = parts.next().unwrap_or("unknown").to_string();
        services.push((unit.to_string(), state));
    }
    services
}

fn parse_blame(text: &str) -> HashMap<String, (String, u64)> {
    let mut services = HashMap::new();
    for line in text.lines() {
        let mut parts: Vec<&str> = line.split_whitespace().collect();
        let Some(unit) = parts.pop() else {
            continue;
        };
        if !unit.ends_with(".service") || parts.is_empty() {
            continue;
        }
        let boot_time = parts.join(" ");
        let boot_secs = duration_secs(&boot_time);
        services.insert(unit.to_string(), (boot_time, boot_secs));
    }
    services
}

fn parse_active(text: &str) -> HashMap<String, String> {
    let mut services = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let mut unit = parts.next();
        if unit.is_some_and(|value| !value.ends_with(".service")) {
            unit = parts.next();
        }
        let Some(unit) = unit else {
            continue;
        };
        if !unit.ends_with(".service") {
            continue;
        }
        let _load = parts.next();
        let active = parts.next().unwrap_or("unknown").to_string();
        services.insert(unit.to_string(), active);
    }
    services
}

pub fn duration_secs(text: &str) -> u64 {
    let mut total = 0.0;
    for part in text.split_whitespace() {
        if let Some(value) = part.strip_suffix("ms") {
            if let Ok(value) = value.parse::<f64>() {
                total += value / 1000.0;
            }
        } else if let Some(value) = part.strip_suffix("min") {
            if let Ok(value) = value.parse::<f64>() {
                total += value * 60.0;
            }
        } else if let Some(value) = part.strip_suffix('h') {
            if let Ok(value) = value.parse::<f64>() {
                total += value * 3600.0;
            }
        } else if let Some(value) = part.strip_suffix('s') {
            if let Ok(value) = value.parse::<f64>() {
                total += value;
            }
        }
    }
    total as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemd_durations() {
        assert_eq!(duration_secs("3min 33.547s"), 213);
        assert_eq!(duration_secs("13min 10.223s"), 790);
        assert_eq!(duration_secs("500ms"), 0);
        assert_eq!(duration_secs("1h 2min 3s"), 3723);
        assert_eq!(duration_secs(""), 0);
    }

    #[test]
    fn parses_blame_lines_with_multi_part_times() {
        let text = "13min 10.223s plocate-updatedb.service\n 1min 42.971s mariadb.service\n 40.934s dev-sda2.device\n";
        let parsed = parse_blame(text);
        assert_eq!(
            parsed.get("plocate-updatedb.service"),
            Some(&("13min 10.223s".to_string(), 790))
        );
        assert_eq!(
            parsed.get("mariadb.service"),
            Some(&("1min 42.971s".to_string(), 102))
        );
        assert!(!parsed.contains_key("dev-sda2.device"));
    }

    #[test]
    fn parses_unit_file_states() {
        let text = "abrtd.service enabled enabled\ncups.service disabled disabled\nplymouth-quit-wait.service static -\n";
        let parsed = parse_unit_files(text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(
            parsed[0],
            ("abrtd.service".to_string(), "enabled".to_string())
        );
        assert_eq!(
            parsed[2],
            (
                "plymouth-quit-wait.service".to_string(),
                "static".to_string()
            )
        );
    }

    #[test]
    fn parses_active_states_with_optional_marker() {
        let text = "  abrt-oops.service loaded inactive dead ABRT kernel log watcher\n● abrtd.service loaded failed failed ABRT Daemon\n";
        let parsed = parse_active(text);
        assert_eq!(
            parsed.get("abrt-oops.service"),
            Some(&"inactive".to_string())
        );
        assert_eq!(parsed.get("abrtd.service"), Some(&"failed".to_string()));
    }

    #[test]
    fn rejects_units_that_are_not_plain_service_names() {
        assert!(apply_action("../etc/passwd", ServiceAction::Disable).is_err());
        assert!(apply_action("mariadb.socket", ServiceAction::Disable).is_err());
    }
}
