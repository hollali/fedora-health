# fedora-slowdown

Diagnose and fix a Fedora system that feels slow because of a spinning disk:
I/O pressure sampling, startup benchmarking, a startup audit, safe system
checks, and a terminal dashboard.

The tools are deliberately conservative:

- `fedora-doctor.sh` is read-only by default. It never changes the system
  unless you explicitly run `--fix` or `--fix-id`.
- Every fix is idempotent, guarded, and prints the exact command it will run
  under `--dry-run`.
- `hdd-watch` benchmarks apps by launching them, recording disk/CPU/wait
  behaviour, and then closing the process group it started.
- Prewarming refuses to load more than 25% of `MemAvailable`.
- Nothing is downloaded, removed, or enabled automatically.

## Install

Build the Rust binary as your normal user:

```bash
cargo build --release --manifest-path hdd-watch/Cargo.toml
```

Then install the binary, doctor script, systemd units and an example prewarm
config:

```bash
sudo ./install.sh
```

`install.sh` copies files and runs `systemctl daemon-reload` only. It does not
enable or start services and does not edit your prewarm config if one already
exists.

Without installing, run everything from this directory:

```bash
./fedora-doctor.sh
./hdd-watch/target/release/hdd-watch --help
```

## Doctor: what is slow and why

Read-only report:

```bash
./fedora-doctor.sh
```

Machine-readable output for scripts and the TUI:

```bash
./fedora-doctor.sh --json
```

Read-only checks work without root; some details (SMART attributes, other
users' processes) need root and are reported as unknown otherwise.

Fixes require root:

```bash
sudo ./fedora-doctor.sh --dry-run          # preview every safe fix
sudo ./fedora-doctor.sh --fix-id io-scheduler --dry-run
sudo ./fedora-doctor.sh --fix-id io-scheduler
sudo ./fedora-doctor.sh --fix              # all applicable safe fixes
```

Fix IDs:

| ID | Action |
| --- | --- |
| `filesystem-btrfs-space` | Background btrfs balance, only if unallocated space is very low |
| `io-scheduler` | Use `bfq` on rotational disks that support it |
| `junk-journald` | Vacuum journals to 200 MB only if they exceed 1 GB |
| `junk-dnf` | Clean dnf metadata only if it exceeds 500 MB |
| `junk-coredumps` | Remove old coredumps after reporting the count |
| `junk-flatpak` | Remove unused Flatpak runtimes |
| `services-indexers` | Stop running tracker/baloo indexers for this session |
| `kernel-tuning` | Install the HDD sysctl profile |
| `startup-fontcache` | Rebuild the font cache |
| `startup-iconcache` | Rebuild icon/desktop caches |
| `startup-flatpak` | Repair Flatpak state and stale refs |
| `startup-ldconfig` | Refresh the shared-library cache |

Review the dry-run output before applying anything. In particular, a btrfs
balance can take a long time and should be done on a backed-up, idle machine.

## Sampler and reports

Record one sample every 5 seconds:

```bash
sudo hdd-watch watch 5
```

Quick single sample:

```bash
sudo hdd-watch snap 3
```

Analyse the recorded log:

```bash
sudo hdd-watch report
```

The report shows averages, the worst moments, the processes that were read
heaviest during pressure, trend changes, and swap-thrashing hints. Logs live
in `/var/log/hdd-watch.log` for root and in the per-user state directory for
non-root runs.

Run the sampler as a service:

```bash
sudo systemctl enable --now hdd-watch.service
```

## Benchmark app starts

Warm benchmark (the app must not already be open):

```bash
hdd-watch launch -- firefox
```

Cold benchmark (requires root, drops the page cache for the whole machine):

```bash
sudo hdd-watch launch --cold -- firefox
```

Useful options:

```text
--runs N        run several times
--timeout SECS  maximum time to wait for the app to settle
--keep          leave the app running instead of closing it
--trace         run under strace and group opened files by directory
--after-boot    tag the run as a first-launch without dropping caches
```

The tool starts the app, watches its process group until CPU/disk activity
settles, records wall time, CPU time, bytes read, major faults, D-state and
block-I/O wait, then closes what it started. History:

```bash
hdd-watch launches
```

An app that was already running is detected and reported instead of producing
a misleading time.

## Prewarm the right apps

Find apps that were much slower cold than warm:

```bash
hdd-watch prewarm --suggest
```

Copy only those entries into `/etc/hdd-watch/prewarm.conf`, or add the selected
app from the TUI. Test:

```bash
sudo hdd-watch prewarm
```

The prewarm worker runs at idle I/O priority and refuses to load more than 25%
of `MemAvailable`. Enable it to warm configured entries when the graphical
session starts:

```bash
sudo systemctl enable hdd-watch-prewarm.service
```

## Audit login startup

```bash
hdd-watch apps
```

Lists autostart entries, user units, and processes started in the first two
minutes after login, sorted by disk read. Nothing is disabled automatically;
use it to decide what to remove or delay.

## TUI

```bash
sudo hdd-watch tui
hdd-watch tui --demo          # fake data, no system changes
hdd-watch tui --interval 5
```

Tabs: **Live**, **History**, **Doctor**, **Disk**, **Launch**.

- `Tab` / `Shift+Tab` or `1`..`5`: switch tabs
- arrows or `hjkl`: move/scroll
- `?`: help, `q`: quit
- Live: `s` sort processes, space pause
- History: `h`/`l` scroll, `+`/`-` zoom
- Doctor: `r` re-check, `f` preview and confirm a fix
- Disk: `r` reload
- Launch: `/` filter apps, `Enter` warm benchmark, `c` cold benchmark, `p`
  add to prewarm.conf

Fixes, benchmarks, cold cache drops and prewarm edits always show a
confirmation popup first. Without root the TUI remains read-only and explains
which actions are unavailable.

## Environment overrides

Useful for tests and per-user runs:

```text
HDD_WATCH_LOG            sample log path
HDD_WATCH_STATE          state directory
HDD_WATCH_LAUNCHES       launch history path
HDD_WATCH_PREWARM_CONF   prewarm config path
HDD_WATCH_DOCTOR         path to fedora-doctor.sh
```

## Safety notes

- Back up important data before filesystem maintenance or a btrfs balance.
- Read each `--dry-run` line; the guards reduce risk but do not know your
  workflow.
- Cold benchmarks intentionally make the machine slow for a moment.
- Prewarming does not defragment and does not tune the disk firmware.
- If SMART shows reallocated or pending sectors, back up first; software
  tuning cannot repair failing hardware.
- `install.sh` does not enable services. Enabling them is an explicit choice.

## Tests

```bash
cd hdd-watch
cargo test
cargo test --no-default-features
cargo clippy --all-targets -- -D warnings
```

The integration tests run real app launches (`sleep`, `sh` reading
`/usr/bin/*`) and validate `fedora-doctor.sh --json`, so they need a normal
Linux userspace. The doctor JSON test also uses `python3`.
