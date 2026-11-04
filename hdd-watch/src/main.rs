mod apps;
mod doctor;
mod launch;
mod logfmt;
mod prewarm;
mod report;
mod sampler;
#[cfg(feature = "tui")]
mod services;
#[cfg(feature = "tui")]
mod tui;

use std::process::ExitCode;

const USAGE: &str = "\
hdd-watch — diagnose and watch slow disks on Fedora

Usage:
  hdd-watch watch [interval_secs]     Record a sample every N seconds (default 5) to /var/log/hdd-watch.log
  hdd-watch snap [secs]               Take one sample (default 3 seconds) and print it
  hdd-watch report                    Analyse the sample log: averages, worst moments, culprits, trend
  hdd-watch launch [options] -- CMD   Benchmark one app start
  hdd-watch launches                  Show app startup history (first/latest, cold/warm, slowdowns)
  hdd-watch apps                      Audit login autostart, user units and first-2-minute disk reads
  hdd-watch prewarm [--suggest]       Warm configured apps into the page cache
  hdd-watch tui [--demo] [--interval N]
                                      Terminal dashboard (Tab/1-6 switch tabs, ? for help)

launch options:
  --cold          Drop the page cache first (root), then run cold, warm, warm
  --runs N        Number of runs (default 1)
  --keep          Do not terminate the process tree after settling
  --trace         Run under strace and group opened files by directory
  --after-boot    Tag the run as a first-launch (no cache drop)
  --timeout SECS  Maximum bench time (default 60)

Environment overrides (mainly for tests):
  HDD_WATCH_LOG, HDD_WATCH_STATE, HDD_WATCH_LAUNCHES,
  HDD_WATCH_PREWARM_CONF, HDD_WATCH_DOCTOR
";

fn parse_u64(value: Option<&String>, what: &str) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("{what} needs a value"))?;
    value
        .parse::<u64>()
        .map_err(|_| format!("invalid {what}: {value}"))
}

fn warn_not_root(command: &str) {
    if !logfmt::is_root() && !matches!(command, "tui" | "help" | "--help" | "-h") {
        eprintln!(
            "hdd-watch: warning: not root; processes owned by other users are invisible. Use sudo for full data."
        );
    }
}

fn parse_launch(args: &[String]) -> Result<launch::LaunchOptions, String> {
    let mut options = launch::LaunchOptions::default();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--" => {
                options.command = args[index + 1..].to_vec();
                index = args.len();
            }
            "--cold" => options.cold = true,
            "--keep" => options.keep = true,
            "--trace" => options.trace = true,
            "--after-boot" => options.after_boot = true,
            "--runs" => {
                index += 1;
                options.runs = parse_u64(args.get(index), "--runs")?.min(u32::MAX as u64) as u32;
            }
            "--timeout" => {
                index += 1;
                options.timeout_secs = parse_u64(args.get(index), "--timeout")?;
            }
            other if other.starts_with("--runs=") => {
                options.runs = other[7..]
                    .parse::<u32>()
                    .map_err(|_| format!("invalid --runs: {other}"))?;
            }
            other if other.starts_with("--timeout=") => {
                options.timeout_secs = other[10..]
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --timeout: {other}"))?;
            }
            "-h" | "--help" => return Err("help".to_string()),
            other => return Err(format!("unknown launch option: {other}")),
        }
        index += 1;
    }
    if options.command.is_empty() {
        return Err("launch needs a command after --".to_string());
    }
    if options.cold && options.after_boot {
        return Err("--cold and --after-boot cannot be combined".to_string());
    }
    if options.runs == 0 {
        options.runs = 1;
    }
    Ok(options)
}

fn print_snap(sample: &logfmt::LogSample) {
    println!(
        "io_psi {:.1}%   mem_psi {:.1}%   iowait {:.1}%   free RAM {} MB   swap {} MB",
        sample.io_psi, sample.mem_psi, sample.iowait_pct, sample.mem_avail_mb, sample.swap_used_mb
    );
    if sample.procs.is_empty() {
        println!("top processes: none above the reporting threshold");
    } else {
        println!("top processes by disk I/O:");
        for proc in &sample.procs {
            println!("  {:>10.1} KB/s  {}", proc.kbps, proc.name);
        }
    }
}

fn run_command(args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return Ok(());
    };
    warn_not_root(command);
    match command {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "watch" => {
            let interval = args
                .get(1)
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid interval: {value}"))
                })
                .transpose()?
                .unwrap_or(5);
            sampler::watch_loop(interval);
        }
        "snap" => {
            let seconds = args
                .get(1)
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid seconds: {value}"))
                })
                .transpose()?
                .unwrap_or(3);
            let sample = sampler::snap(seconds);
            print_snap(&sample);
            Ok(())
        }
        "report" => {
            let path = logfmt::default_log_path();
            let samples = logfmt::read_samples(&path);
            print!(
                "{}",
                report::render_report(&samples, logfmt::now_epoch(), &path.display().to_string())
            );
            Ok(())
        }
        "launch" => {
            let options = match parse_launch(&args[1..]) {
                Ok(options) => options,
                Err(err) if err == "help" => {
                    print!("{USAGE}");
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            let mut progress = |_: launch::Progress| {};
            let result = launch::run(&options, &mut progress)?;
            launch::print_report(&result);
            Ok(())
        }
        "launches" => {
            print!("{}", launch::render_launches());
            Ok(())
        }
        "apps" => {
            print!("{}", apps::render_audit());
            Ok(())
        }
        "prewarm" => {
            let mut suggest = false;
            let mut worker = None;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--suggest" => suggest = true,
                    "--worker" => {
                        index += 1;
                        worker = Some(
                            args.get(index)
                                .ok_or_else(|| "--worker needs a manifest path".to_string())?
                                .clone(),
                        );
                    }
                    other => return Err(format!("unknown prewarm option: {other}")),
                }
                index += 1;
            }
            if let Some(manifest) = worker {
                prewarm::run_worker(std::path::Path::new(&manifest))
            } else {
                prewarm::run(suggest)
            }
        }
        "tui" => {
            #[cfg(feature = "tui")]
            {
                let mut demo = false;
                let mut interval = 2u64;
                let mut index = 1;
                while index < args.len() {
                    match args[index].as_str() {
                        "--demo" => demo = true,
                        "--interval" => {
                            index += 1;
                            interval = args
                                .get(index)
                                .ok_or_else(|| "--interval needs a value".to_string())?
                                .parse()
                                .map_err(|_| "--interval must be a number".to_string())?;
                        }
                        other => return Err(format!("unknown tui option: {other}")),
                    }
                    index += 1;
                }
                tui::run(demo, interval.max(1)).map_err(|err| format!("tui failed: {err}"))
            }
            #[cfg(not(feature = "tui"))]
            {
                Err("this build was compiled without the tui feature".to_string())
            }
        }
        other => Err(format!("unknown command: {other}\n\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    match run_command(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hdd-watch: {err}");
            ExitCode::FAILURE
        }
    }
}
