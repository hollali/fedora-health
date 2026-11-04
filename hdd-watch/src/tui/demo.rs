use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::apps::DesktopEntry;
use crate::doctor::{DiskInfo, Finding, SmartInfo};
use crate::launch::{BenchResult, LaunchReport, Verdict};
use crate::logfmt::{sanitize_name, LaunchRecord, LogSample, ProcKbps};
use crate::report::Severity;
use crate::sampler::{IoRates, LiveSample};
use crate::services::ServiceInfo;

use super::app::AppEvent;

pub fn fake_sample(step: u64, now: u64) -> LiveSample {
    let phase = step as f64 * 0.25;
    let io = (8.0 + phase.sin() * 7.0 + (step % 17) as f64 * 0.8).max(0.0);
    let mem = (2.5 + (phase * 0.7).cos() * 2.0).max(0.0);
    let iowait = (io * 1.3 + 2.0).min(70.0);
    let ram = (2600.0 + (phase * 0.3).sin() * 900.0).max(300.0) as u64;
    let swap = (step % 40) * 22;
    let names = [
        ("firefox", 900.0),
        ("tracker-miner-f", 420.0),
        ("code", 250.0),
        ("gnome-shell", 90.0),
        ("baloo_file", 60.0),
    ];
    let mut rates = Vec::new();
    for (idx, (name, base)) in names.iter().enumerate() {
        let wave = ((phase * 0.9) + idx as f64).sin() * 0.5 + 1.0;
        rates.push(IoRates {
            name: sanitize_name(name),
            read_kbps: base * wave,
            write_kbps: base * 0.2 * (idx as f64 + 1.0),
        });
    }
    let procs = rates
        .iter()
        .take(3)
        .map(|rate| ProcKbps {
            name: rate.name.clone(),
            kbps: rate.total_kbps(),
        })
        .collect();
    let log = LogSample {
        ts: now,
        io_psi: io,
        mem_psi: mem,
        iowait_pct: iowait,
        mem_avail_mb: ram,
        swap_used_mb: swap,
        procs,
    };
    LiveSample { log, rates }
}

pub fn seed_history(now: u64) -> Vec<LogSample> {
    let mut history = Vec::new();
    for index in 0..60u64 {
        let ts = now.saturating_sub((59 - index) * 30);
        let io = 3.0 + index as f64 * 0.22 + (index as f64 * 0.7).sin() * 2.0;
        let ram = 3200u64.saturating_sub(index * 18);
        history.push(LogSample {
            ts,
            io_psi: io.max(0.0),
            mem_psi: (io * 0.25).max(0.0),
            iowait_pct: (io * 0.8).max(0.0),
            mem_avail_mb: ram.max(400),
            swap_used_mb: 100 + index * 8,
            procs: vec![
                ProcKbps {
                    name: "firefox".into(),
                    kbps: 200.0 + index as f64 * 12.0,
                },
                ProcKbps {
                    name: "tracker-miner-f".into(),
                    kbps: 80.0 + index as f64 * 4.0,
                },
            ],
        });
    }
    history
}

pub fn fake_findings() -> Vec<Finding> {
    let items = [
        (
            "disk-health",
            Severity::Warn,
            "Disk health has minor indicators",
            "Reallocated sectors: 2. Back up and watch this disk.",
            false,
        ),
        (
            "filesystem-usage",
            Severity::Ok,
            "Root filesystem usage",
            "62% used on / (btrfs).",
            false,
        ),
        (
            "filesystem-btrfs-space",
            Severity::Warn,
            "btrfs has little unallocated space",
            "812 MB unallocated. A background balance can reclaim chunks.",
            true,
        ),
        (
            "filesystem-mountopts",
            Severity::Warn,
            "Mount options are not HDD-friendly",
            "noatime is missing; every read updates atime.",
            false,
        ),
        (
            "memory-swap",
            Severity::Warn,
            "Memory and swap pressure",
            "Available RAM 1.2 GB, swap used 900 MB, PSI memory 14%.",
            false,
        ),
        (
            "io-scheduler",
            Severity::Bad,
            "Rotational disk is not using bfq",
            "sda: active=mq-deadline, bfq is available.",
            true,
        ),
        (
            "junk-journald",
            Severity::Warn,
            "journald logs are large",
            "1.8 GB of journals. Vacuuming to 200 MB is safe.",
            true,
        ),
        (
            "junk-coredumps",
            Severity::Warn,
            "Many coredumps are stored",
            "420 MB in 88 files.",
            true,
        ),
        (
            "junk-flatpak",
            Severity::Warn,
            "Unused Flatpak runtimes",
            "4 runtime refs are not used by any app.",
            true,
        ),
        (
            "services-failed",
            Severity::Ok,
            "Failed systemd units",
            "None.",
            false,
        ),
        (
            "services-slowboot",
            Severity::Bad,
            "Slow boot",
            "Total 2min 14.203s (2min 1.872s in userspace). Slowest services: docker 1min 55s; mariadb 1min 38s; fwupd 1min 32s. Planned: switch docker to socket activation; switch mariadb to socket activation; run fwupd after login; delay postgresql until after boot.",
            true,
        ),
        (
            "services-indexers",
            Severity::Warn,
            "File indexers are running",
            "tracker-miner-fs-3 is competing with the HDD.",
            true,
        ),
        (
            "kernel-tuning",
            Severity::Warn,
            "Kernel tuning for HDDs is not installed",
            "/etc/sysctl.d/99-hdd.conf is missing.",
            true,
        ),
        (
            "startup-autostart",
            Severity::Warn,
            "Many autostart entries",
            "17 entries start at login.",
            false,
        ),
        (
            "startup-fontcache",
            Severity::Warn,
            "Font cache is stale or slow",
            "fc-list took 812 ms; rebuilding helps.",
            true,
        ),
        (
            "startup-iconcache",
            Severity::Ok,
            "Icon and desktop caches",
            "Caches look fresh.",
            false,
        ),
        (
            "startup-shadercache",
            Severity::Warn,
            "GPU shader cache is disabled",
            "MESA_SHADER_CACHE_DISABLE=true.",
            false,
        ),
        (
            "startup-flatpak",
            Severity::Warn,
            "Flatpak state needs attention",
            "20 unused runtime refs.",
            true,
        ),
        (
            "startup-dns",
            Severity::Warn,
            "DNS lookups are slow or unhealthy",
            "example.com 912 ms; a dead DNS server is configured.",
            false,
        ),
        (
            "startup-browser-profile",
            Severity::Warn,
            "Browser profile is large",
            "5.2 GB; places.sqlite is 240 MB.",
            false,
        ),
        (
            "startup-ldconfig",
            Severity::Warn,
            "Shared-library cache is stale",
            "ld.so.cache is older than installed libraries.",
            true,
        ),
        (
            "startup-dbus-keyring",
            Severity::Warn,
            "D-Bus or keyring is slow",
            "secret service 1400 ms.",
            false,
        ),
    ];
    items
        .iter()
        .map(|(id, severity, title, detail, fix)| Finding {
            id: (*id).to_string(),
            severity: *severity,
            title: (*title).to_string(),
            detail: (*detail).to_string(),
            fix_available: *fix,
        })
        .collect()
}

pub fn startup_text() -> String {
    [
        "autostart (17 entries):",
        "  org.gnome.Software.desktop, update-notifier.desktop, tracker-miner.desktop,",
        "  flatpak-user-update.desktop, gnome-keyring-ssh.desktop (+12 more)",
        "",
        "slowest login units:",
        "  plocate-updatedb.service      10m 46s",
        "  fwupd.service                  2m 57s",
        "  tracker-miner-fs-3.service     1m 12s",
        "",
        "first-2-minute disk readers (demo):",
        "  firefox        512.0 MB   pid 4211",
        "  gnome-shell    188.4 MB   pid 1902",
        "  tracker-miner   96.2 MB   pid 3388",
        "",
        "This is fake demo data. Run the real audit with: hdd-watch apps",
    ]
    .join("\n")
}

pub fn fake_services() -> Vec<ServiceInfo> {
    [
        (
            "plocate-updatedb.service",
            "13min 10.223s",
            790,
            "enabled",
            "active",
        ),
        (
            "mariadb.service",
            "1min 42.971s",
            102,
            "disabled",
            "inactive",
        ),
        (
            "plymouth-quit-wait.service",
            "1min 38.097s",
            98,
            "static",
            "active",
        ),
        ("abrtd.service", "1min 37.137s", 97, "enabled", "failed"),
        ("docker.service", "1min 17.968s", 77, "enabled", "active"),
        (
            "NetworkManager-wait-online.service",
            "6.104s",
            6,
            "disabled",
            "inactive",
        ),
        ("sshd.service", "212ms", 0, "enabled", "active"),
        ("cups.service", "88ms", 0, "disabled", "inactive"),
    ]
    .iter()
    .map(|(unit, boot_time, boot_secs, state, active)| ServiceInfo {
        unit: (*unit).to_string(),
        boot_time: (*boot_time).to_string(),
        boot_secs: *boot_secs,
        state: (*state).to_string(),
        active: (*active).to_string(),
    })
    .collect()
}

pub fn fake_apps() -> Vec<DesktopEntry> {
    [
        ("firefox.desktop", "Firefox", "firefox %u"),
        ("code.desktop", "Visual Studio Code", "code %F"),
        (
            "org.gnome.Nautilus.desktop",
            "Files",
            "nautilus --new-window %U",
        ),
        (
            "com.spotify.Client.desktop",
            "Spotify",
            "flatpak run com.spotify.Client %U",
        ),
    ]
    .iter()
    .map(|(id, name, exec)| DesktopEntry {
        id: (*id).to_string(),
        name: (*name).to_string(),
        exec: (*exec).to_string(),
        no_display: false,
        hidden: false,
        source: std::path::PathBuf::from("/demo/applications").join(id),
    })
    .collect()
}

pub fn fake_disk() -> DiskInfo {
    DiskInfo {
        smart: vec![
            SmartInfo {
                device: "/dev/sda".into(),
                model: "WDC WD10EZEX-08WN4A0".into(),
                health: "PASSED".into(),
                reallocated: Some(2),
                pending: Some(0),
            },
            SmartInfo {
                device: "/dev/sdb".into(),
                model: "Seagate ST2000DM008".into(),
                health: "PASSED".into(),
                reallocated: Some(0),
                pending: Some(0),
            },
        ],
        filesystem: "btrfs".into(),
        usage: "/dev/sda3  465G  289G  176G  63% /".into(),
        mounts: "rw,relatime,compress=zstd:1,space_cache=v2".into(),
        schedulers: vec![
            ("sda".into(), "none mq-deadline kyber [bfq]".into()),
            ("sdb".into(), "none mq-deadline kyber [bfq]".into()),
        ],
    }
}

pub fn fake_launch_history(now: u64) -> Vec<LaunchRecord> {
    let mut records = Vec::new();
    let apps = ["firefox", "code", "nautilus"];
    for (app_index, app) in apps.iter().enumerate() {
        for (run, kind) in ["cold", "warm", "warm", "warm"].iter().enumerate() {
            let base = 2200 + app_index as u64 * 1300;
            let settled = if *kind == "cold" {
                base * 4
            } else {
                base + run as u64 * 80
            };
            records.push(LaunchRecord {
                ts: now.saturating_sub((20 - run as u64) * 600),
                app: (*app).to_string(),
                kind: (*kind).to_string(),
                settled_ms: settled,
                cpu_ms: settled / 2,
                read_bytes: settled * 40_000,
                majflt: settled / 20,
                d_pct: if *kind == "cold" { 35.0 } else { 8.0 },
                blkio_pct: if *kind == "cold" { 22.0 } else { 3.0 },
                procs: 4,
                verdict: if *kind == "cold" {
                    "disk-bound".to_string()
                } else {
                    "waiting".to_string()
                },
                command: if *app == "nautilus" {
                    "nautilus --new-window".to_string()
                } else {
                    format!("{app} --new-window")
                },
            });
        }
    }
    records
}

pub fn fake_launch_report(app: &str, record: LaunchRecord) -> LaunchReport {
    let verdict = if record.verdict == "disk-bound" {
        Verdict::DiskBound
    } else if record.verdict == "cpu-bound" {
        Verdict::CpuBound
    } else {
        Verdict::Waiting
    };
    let result = BenchResult {
        record,
        verdict,
        window_ms: Some(1700),
        timed_out: false,
        trace_dirs: vec![],
    };
    let _ = app;
    LaunchReport {
        results: vec![result],
        already_running: None,
        cold_warm_ratio: None,
    }
}

pub fn spawn_sampler(tx: Sender<AppEvent>, interval_secs: u64) {
    thread::spawn(move || {
        let mut step = 0u64;
        loop {
            thread::sleep(Duration::from_secs(interval_secs.max(1)));
            step = step.saturating_add(1);
            let now = crate::logfmt::now_epoch();
            if tx.send(AppEvent::Sample(fake_sample(step, now))).is_err() {
                break;
            }
        }
    });
}

pub fn spawn_launch(app: String, tx: Sender<AppEvent>, cold: bool) {
    thread::spawn(move || {
        let steps = 12u64;
        for step in 1..=steps {
            thread::sleep(Duration::from_millis(250));
            let _ = tx.send(AppEvent::LaunchProgress(crate::launch::Progress {
                phase: if cold {
                    format!("demo cold {step}/{steps}")
                } else {
                    format!("demo warm {step}/{steps}")
                },
                elapsed_ms: step * 250,
                cpu_ms: step * 90,
                read_bytes: step * 1_800_000,
                majflt: step * 3,
                procs: 4,
                settled: false,
            }));
        }
        let record = LaunchRecord {
            ts: crate::logfmt::now_epoch(),
            app: app.clone(),
            kind: if cold { "cold".into() } else { "warm".into() },
            settled_ms: if cold { 12_400 } else { 3_100 },
            cpu_ms: 1_400,
            read_bytes: 22_000_000,
            majflt: 180,
            d_pct: if cold { 34.0 } else { 9.0 },
            blkio_pct: if cold { 21.0 } else { 4.0 },
            procs: 5,
            verdict: if cold {
                "disk-bound".into()
            } else {
                "waiting".into()
            },
            command: format!("{app} (demo)"),
        };
        let report = fake_launch_report(&app, record);
        let _ = tx.send(AppEvent::LaunchDone(Ok(report)));
    });
}

pub fn spawn_fix(id: String, tx: Sender<AppEvent>) {
    thread::spawn(move || {
        let lines = [
            format!("demo: would run fix {id}"),
            "demo: checking preconditions...".to_string(),
            "demo: applying idempotent change...".to_string(),
            "demo: done (nothing was actually changed)".to_string(),
        ];
        for line in lines {
            thread::sleep(Duration::from_millis(350));
            let _ = tx.send(AppEvent::FixLine(line));
        }
        let _ = tx.send(AppEvent::FixDone(Ok(())));
    });
}
