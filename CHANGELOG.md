# changelog

All notable changes to this project will be documented in this file.

## Unreleased

- Initial release
- UX improvements: intuitive diagnostic IDs, no stderr noise, Doctor tab fix-all and auto re-check, tab entry refresh
  - fedora-doctor.sh: diagnose and safely fix common causes of slow Fedora systems
  - fedora-doctor.sh: report total boot time and offer a safe slow-boot fix (`services-slowboot`)
  - fedora-doctor.sh: the slow-boot fix now acts on the measured `systemd-analyze blame` bottlenecks: socket-activates Docker/MariaDB, defers PostgreSQL/PHP-FPM/ABRT by timer, and stops fwupd from blocking GDM
  - hdd-watch: TUI sampler, benchmark, and startup auditor
  - hdd-watch: Services tab to list systemd units by boot time and enable,
    disable, mask, or unmask them with confirmation (requires root)
  - install.sh: system-wide installation script
  - systemd service files for hdd-watch and hdd-watch-prewarm