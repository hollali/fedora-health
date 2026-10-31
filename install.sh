#!/usr/bin/env bash
# Install hdd-watch and fedora-doctor.sh system-wide.
#
# This script only copies files and runs systemctl daemon-reload. It does NOT
# enable services, apply fixes, edit prewarm.conf or drop caches. Those remain
# explicit actions for the administrator.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="$ROOT/hdd-watch"
BIN="${HDD_WATCH_BIN:-$CRATE/target/release/hdd-watch}"
PREFIX="${PREFIX:-/usr/local}"
SBIN="$PREFIX/bin"
UNIT_DIR="/etc/systemd/system"
CONF_DIR="/etc/hdd-watch"

if [[ ${EUID} -ne 0 ]]; then
    echo "install.sh must run as root: sudo $0" >&2
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "release binary not found at $BIN" >&2
    echo "Build it first (as your user, not root):" >&2
    echo "  cargo build --release --manifest-path \"$CRATE/Cargo.toml\"" >&2
    echo "or set HDD_WATCH_BIN=/path/to/binary." >&2
    exit 1
fi

install -d -m 0755 "$SBIN" "$UNIT_DIR" "$CONF_DIR" "$PREFIX/share/doc/hdd-watch"
install -d -m 0755 /var/lib/hdd-watch

install -m 0755 "$BIN" "$SBIN/hdd-watch"
install -m 0755 "$ROOT/fedora-doctor.sh" "$SBIN/fedora-doctor.sh"
install -m 0644 "$ROOT/hdd-watch.service" "$UNIT_DIR/hdd-watch.service"
install -m 0644 "$ROOT/hdd-watch-prewarm.service" "$UNIT_DIR/hdd-watch-prewarm.service"
install -m 0644 "$ROOT/README.md" "$PREFIX/share/doc/hdd-watch/README.md"

if [[ ! -e "$CONF_DIR/prewarm.conf" ]]; then
    install -m 0644 "$ROOT/prewarm.conf.example" "$CONF_DIR/prewarm.conf"
fi

systemctl daemon-reload

cat <<EOF
Installed:
  $SBIN/hdd-watch
  $SBIN/fedora-doctor.sh
  $UNIT_DIR/hdd-watch.service
  $UNIT_DIR/hdd-watch-prewarm.service
  $CONF_DIR/prewarm.conf

Nothing was enabled or started. Review the configuration, then opt in:

  sudo systemctl enable --now hdd-watch.service
  sudo systemctl enable hdd-watch-prewarm.service   # optional, after editing prewarm.conf

Quick checks:
  hdd-watch --help
  sudo $SBIN/fedora-doctor.sh
  sudo hdd-watch tui
EOF
