#!/usr/bin/env bash
# fedora-doctor.sh — report and safely fix common causes of slow Fedora systems.
# Report-only by default. Never deletes documents, never edits /etc/fstab.
set -uo pipefail

PROG=${0##*/}
VERSION="0.1.0"

MODE="report"
JSON=0
DRY_RUN=0
FIX_ID=""

declare -a FIND_IDS=()
declare -a FIND_SEV=()
declare -a FIND_TITLE=()
declare -a FIND_DETAIL=()
declare -a FIND_FIX=()

usage() {
    cat <<'EOF'
Usage: fedora-doctor.sh [options]

  (no options)        report only; changes nothing
  --fix               apply all safe, idempotent fixes
  --dry-run           print the exact commands --fix would run
  --json              print one JSON array of findings on stdout and nothing else
  --fix-id ID         run exactly one named fix (for the TUI)
  --help              show this help

Read-only modes can run unprivileged with reduced visibility. --fix and
--fix-id require root.
EOF
}

while (($# > 0)); do
    case "$1" in
        --fix) MODE="fix" ;;
        --dry-run) DRY_RUN=1; MODE="fix" ;;
        --json) JSON=1 ;;
        --fix-id)
            shift
            if (($# == 0)); then
                printf '%s: --fix-id needs an id\n' "$PROG" >&2
                exit 2
            fi
            FIX_ID=$1
            MODE="fix"
            ;;
        --help | -h)
            usage
            exit 0
            ;;
        *)
            printf '%s: unknown option: %s\n\n' "$PROG" "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if ((JSON == 1)) && [[ $MODE == "fix" ]]; then
    printf '%s: --json cannot be combined with --fix/--fix-id\n' "$PROG" >&2
    exit 2
fi
if [[ $MODE == "fix" ]] && ((DRY_RUN == 0)) && [[ ${EUID:-$(id -u)} -ne 0 ]]; then
    printf '%s: --fix and --fix-id must run as root (use sudo)\n' "$PROG" >&2
    exit 1
fi

if [[ -n ${SUDO_USER:-} ]]; then
    REAL_USER=$SUDO_USER
else
    REAL_USER=$(id -un 2>/dev/null || printf 'root')
fi
REAL_UID=$(id -u "$REAL_USER" 2>/dev/null || printf '0')
REAL_HOME=$(getent passwd "$REAL_USER" 2>/dev/null | cut -d: -f6)
if [[ -z ${REAL_HOME:-} ]]; then
    REAL_HOME=$HOME
fi
XDG_RUNTIME="/run/user/$REAL_UID"

if [[ $MODE != "fix" ]] && ((EUID != 0)); then
    printf '%s: note: running unprivileged; some checks are partial. Use sudo for full data and fixes.\n' "$PROG" >&2
fi

if [[ -t 1 ]] && ((JSON == 0)); then
    C_OK=$'\033[32m'
    C_WARN=$'\033[33m'
    C_BAD=$'\033[31m'
    C_DIM=$'\033[2m'
    C_OFF=$'\033[0m'
else
    C_OK="" C_WARN="" C_BAD="" C_DIM="" C_OFF=""
fi

have() {
    command -v "$1" >/dev/null 2>&1
}

add_finding() {
    FIND_IDS+=("$1")
    FIND_SEV+=("$2")
    FIND_TITLE+=("$3")
    FIND_DETAIL+=("$4")
    FIND_FIX+=("$5")
}

json_escape() {
    local s=$1
    local out=""
    local i ch
    for ((i = 0; i < ${#s}; i++)); do
        ch=${s:i:1}
        case "$ch" in
            "\\") out+="\\\\" ;;
            '"') out+='\"' ;;
            $'\n') out+='\n' ;;
            $'\r') out+='\r' ;;
            $'\t') out+='\t' ;;
            *) out+=$ch ;;
        esac
    done
    printf '%s' "$out"
}

emit_json() {
    local i first=1
    printf '['
    for i in "${!FIND_IDS[@]}"; do
        if ((first == 0)); then
            printf ','
        fi
        first=0
        printf '{"id":"%s","severity":"%s","title":"%s","detail":"%s","fix_available":%s}' \
            "$(json_escape "${FIND_IDS[$i]}")" \
            "$(json_escape "${FIND_SEV[$i]}")" \
            "$(json_escape "${FIND_TITLE[$i]}")" \
            "$(json_escape "${FIND_DETAIL[$i]}")" \
            "$([[ ${FIND_FIX[$i]} == "1" ]] && printf true || printf false)"
    done
    printf ']\n'
}

fmt_mb() {
    local mb=$1
    awk -v v="$mb" 'BEGIN { if (v >= 1024) printf "%.1f GB", v/1024; else printf "%d MB", v }'
}

dir_mb() {
    local total=0 part
    while IFS= read -r part; do
        total=$((total + part))
    done < <(du -sm "$@" 2>/dev/null | awk '{print $1}')
    printf '%s' "$total"
}

now_ms() {
    date +%s%3N
}

time_ms() {
    local start end
    start=$(now_ms)
    "$@" >/dev/null 2>&1
    end=$(now_ms)
    printf '%s' "$((end - start))"
}

trim() {
    local value=$1
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    printf '%s' "$value"
}

as_real_user() {
    if [[ ${EUID:-$(id -u)} -eq 0 && $REAL_USER != "root" ]]; then
        sudo -u "$REAL_USER" env HOME="$REAL_HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME" "$@"
    else
        env HOME="$REAL_HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME" "$@"
    fi
}

print_user_sudo_prefix() {
    if [[ ${EUID:-$(id -u)} -eq 0 && $REAL_USER != "root" ]]; then
        printf 'sudo -u %q env XDG_RUNTIME_DIR=%q ' "$REAL_USER" "$XDG_RUNTIME"
    else
        printf 'env XDG_RUNTIME_DIR=%q ' "$XDG_RUNTIME"
    fi
}

# ---------------------------------------------------------------------------
# 1. Disk health
# ---------------------------------------------------------------------------
check_disk_health() {
    if ! have smartctl; then
        add_finding "disk-health" "warn" "SMART tools missing" \
            "smartctl is not installed; disk health cannot be checked. Install smartmontools." "0"
        return
    fi
    local found=0 problem=0 severe=0 name kind dev out health realloc pending detail=""
    while read -r name kind; do
        [[ $kind == "disk" ]] || continue
        [[ $name == loop* || $name == zram* ]] && continue
        found=1
        dev="/dev/$name"
        out=$(smartctl -H -A "$dev" 2>&1) || true
        if printf '%s' "$out" | grep -qi "overall-health.*FAILED"; then
            health="FAILED"
            severe=1
        elif printf '%s' "$out" | grep -qi "overall-health.*PASSED"; then
            health="PASSED"
        else
            health="unknown"
            problem=1
        fi
        realloc=$(printf '%s\n' "$out" | awk '$1 == "5" {v=$NF; gsub(/[^0-9]/, "", v); print v; exit}')
        pending=$(printf '%s\n' "$out" | awk '$1 == "197" {v=$NF; gsub(/[^0-9]/, "", v); print v; exit}')
        realloc=${realloc:-0}
        pending=${pending:-0}
        if ((realloc > 0 || pending > 0)); then
            severe=1
        fi
        detail+="$name: $health, reallocated=$realloc, pending=$pending. "
    done < <(lsblk -dno NAME,TYPE 2>/dev/null)
    if ((found == 0)); then
        add_finding "disk-health" "ok" "Disk health" "No physical disks found (only zram/loop)." "0"
    elif ((severe == 1)); then
        add_finding "disk-health" "bad" "Disk has failing SMART indicators" \
            "$detail Back up your data now and replace this disk. No software fix can repair it." "0"
    elif ((problem == 1)); then
        add_finding "disk-health" "warn" "Disk SMART status is not fully readable" \
            "$detail Run this check as root (sudo) or install smartmontools for complete data." "0"
    else
        add_finding "disk-health" "ok" "Disk health" "$detail" "0"
    fi
}

# ---------------------------------------------------------------------------
# 2. Filesystem
# ---------------------------------------------------------------------------
check_filesystem() {
    local fs opts usage rota rotational=0
    fs=$(findmnt -n -o FSTYPE / 2>/dev/null) || fs="unknown"
    opts=$(findmnt -n -o OPTIONS / 2>/dev/null) || opts=""
    fs=$(trim "$fs")
    opts=$(trim "$opts")
    usage=$(df -P / 2>/dev/null | awk 'NR == 2 {gsub(/%/, "", $5); print $5}')
    usage=${usage:-0}
    while read -r _ rota; do
        [[ ${rota:-0} == "1" ]] && rotational=1
    done < <(lsblk -dno NAME,ROTA 2>/dev/null)

    if ((usage >= 90)); then
        add_finding "filesystem-usage" "bad" "Root filesystem is almost full" \
            "${usage}% used on / ($fs). Free space or move data; a full HDD fragments badly." "0"
    elif ((usage >= 80)); then
        add_finding "filesystem-usage" "warn" "Root filesystem is getting full" \
            "${usage}% used on / ($fs). Aim to stay below 80%." "0"
    else
        add_finding "filesystem-usage" "ok" "Root filesystem usage" \
            "${usage}% used on / ($fs)." "0"
    fi

    case "$fs" in
        btrfs)
            if have btrfs; then
                local unallocated
                unallocated=$(btrfs filesystem usage -b / 2>/dev/null | awk -F: '/Device unallocated:/ {gsub(/[^0-9]/, "", $2); print $2; exit}')
                unallocated=${unallocated:-0}
                if ((unallocated > 0 && unallocated < 1073741824)); then
                    add_finding "filesystem-btrfs-space" "warn" "btrfs has little unallocated space" \
                        "$(fmt_mb $((unallocated / 1048576))) unallocated. A balance can reclaim chunks; it runs in the background and may take a while." "1"
                elif ((unallocated == 0)); then
                    add_finding "filesystem-btrfs-space" "warn" "btrfs unallocated space could not be read" \
                        "Run as root to read btrfs chunk allocation, or check 'btrfs filesystem usage /'." "0"
                else
                    add_finding "filesystem-btrfs-space" "ok" "btrfs unallocated space" \
                        "$(fmt_mb $((unallocated / 1048576))) unallocated." "0"
                fi
            else
                add_finding "filesystem-btrfs-space" "warn" "btrfs tools missing" \
                    "btrfs-progs is not installed; cannot inspect chunk allocation." "0"
            fi
            ;;
        ext4)
            if have e4defrag; then
                local score
                score=$(e4defrag -c / 2>/dev/null | awk '/Fragmentation score/ {print $NF; exit}')
                score=${score:-0}
                if ((score >= 30)); then
                    add_finding "filesystem-ext4-frag" "warn" "ext4 root filesystem is fragmented" \
                        "e4defrag fragmentation score is $score/100. Defragmenting can help but may be slow; back up first." "0"
                else
                    add_finding "filesystem-ext4-frag" "ok" "ext4 fragmentation" \
                        "e4defrag fragmentation score is $score/100." "0"
                fi
            else
                add_finding "filesystem-ext4-frag" "warn" "e4defrag missing" \
                    "Install e2fsprogs to measure ext4 fragmentation." "0"
            fi
            ;;
        xfs)
            add_finding "filesystem-ext4-frag" "ok" "xfs filesystem" \
                "xfs does not use ext4 defragmentation; online fragmentation checks are not available here." "0"
            ;;
        *)
            add_finding "filesystem-unknown" "warn" "Unrecognised root filesystem" \
                "Root filesystem type is '${fs:-unknown}'; only btrfs, ext4 and xfs branch advice is available." "0"
            ;;
    esac

    local mount_detail
    mount_detail="Mount options for /: $opts"
    if [[ ,$opts, != *,noatime,* ]]; then
        mount_detail+=" | noatime is missing: every read updates atime and causes extra writes on an HDD. This is advice only; edit /etc/fstab yourself."
        add_finding "filesystem-mountopts" "warn" "Mount options are not HDD-friendly" "$mount_detail" "0"
    elif [[ $fs == "btrfs" ]] && ((rotational == 1)) && [[ ,$opts, != *,autodefrag,* ]]; then
        mount_detail+=" | btrfs on a rotating disk would benefit from autodefrag. Advice only; edit /etc/fstab yourself."
        add_finding "filesystem-mountopts" "warn" "btrfs autodefrag is not enabled" "$mount_detail" "0"
    else
        add_finding "filesystem-mountopts" "ok" "Mount options" "$mount_detail" "0"
    fi
}

fix_filesystem_btrfs_space() {
    local log="/var/log/hdd-watch-balance.log"
    if [[ $(findmnt -n -o FSTYPE / 2>/dev/null) != "btrfs" ]]; then
        printf 'skipping btrfs balance: / is not btrfs\n'
        return
    fi
    if ! have btrfs; then
        printf 'skipping btrfs balance: btrfs command not found\n'
        return
    fi
    local unallocated
    unallocated=$(btrfs filesystem usage -b / 2>/dev/null | awk -F: '/Device unallocated:/ {gsub(/[^0-9]/, "", $2); print $2; exit}')
    unallocated=${unallocated:-0}
    if ((unallocated == 0 || unallocated >= 1073741824)); then
        printf 'skipping btrfs balance: unallocated space is %s (not needed)\n' "$(fmt_mb $((unallocated / 1048576)))"
        return
    fi
    if pgrep -f 'btrfs balance start.*-dusage=30' >/dev/null 2>&1; then
        printf 'btrfs balance already running\n'
        return
    fi
    local cmd="nohup btrfs balance start -dusage=30 -musage=30 / >>$log 2>&1 &"
    if ((DRY_RUN == 1)); then
        printf '%s\n' "$cmd"
        return
    fi
    printf 'starting background btrfs balance; log: %s\n' "$log"
    nohup btrfs balance start -dusage=30 -musage=30 / >>"$log" 2>&1 &
}

# ---------------------------------------------------------------------------
# 3. Memory and swap
# ---------------------------------------------------------------------------
check_memory() {
    local mem_total mem_avail swap_total swap_free swap_used swappiness mem_psi io_psi zram=0 avail_mb
    mem_total=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null)
    mem_avail=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo 2>/dev/null)
    swap_total=$(awk '/^SwapTotal:/ {print $2}' /proc/meminfo 2>/dev/null)
    swap_free=$(awk '/^SwapFree:/ {print $2}' /proc/meminfo 2>/dev/null)
    mem_total=${mem_total:-0} mem_avail=${mem_avail:-0} swap_total=${swap_total:-0} swap_free=${swap_free:-0}
    swap_used=$(((swap_total - swap_free) / 1024))
    avail_mb=$((mem_avail / 1024))
    swappiness=$(trim "$(cat /proc/sys/vm/swappiness 2>/dev/null)") || swappiness="unknown"
    mem_psi=$(awk '/^some / {for (i = 1; i <= NF; i++) if ($i ~ /^avg10=/) {split($i, a, "="); print a[2]; exit}}' /proc/pressure/memory 2>/dev/null)
    io_psi=$(awk '/^some / {for (i = 1; i <= NF; i++) if ($i ~ /^avg10=/) {split($i, a, "="); print a[2]; exit}}' /proc/pressure/io 2>/dev/null)
    mem_psi=${mem_psi:-0} io_psi=${io_psi:-0}
    [[ -d /sys/block/zram0 ]] && zram=1
    local severity="ok"
    if awk -v s="$swap_used" -v m="$mem_psi" 'BEGIN {exit !(s > 2000 || m >= 30)}'; then
        severity="bad"
    elif awk -v s="$swap_used" -v m="$mem_psi" 'BEGIN {exit !(s > 500 || m >= 10)}'; then
        severity="warn"
    fi
    add_finding "memory-swap" "$severity" "Memory and swap pressure" \
        "Available RAM $(fmt_mb "$avail_mb"), swap used $(fmt_mb "$swap_used") of $(fmt_mb $((swap_total / 1024))), swappiness $swappiness, zram $([[ $zram -eq 1 ]] && printf 'yes' || printf 'no'), PSI memory ${mem_psi}%, PSI io ${io_psi}%." "0"
}

# ---------------------------------------------------------------------------
# 4. I/O scheduler
# ---------------------------------------------------------------------------
check_scheduler() {
    if ! have lsblk; then
        add_finding "io-scheduler" "warn" "Cannot inspect I/O schedulers" "lsblk is missing." "0"
        return
    fi
    local detail="" need_fix=0 found=0 name rota sched
    while read -r name rota; do
        [[ ${rota:-0} == "1" ]] || continue
        found=1
        sched=$(cat "/sys/block/$name/queue/scheduler" 2>/dev/null) || sched=""
        local active
        active=$(printf '%s' "$sched" | sed -n 's/.*\[\([^]]*\)\].*/\1/p')
        detail+="$name: active=${active:-unknown}, available=${sched:-unknown}. "
        if [[ $active != "bfq" ]]; then
            if [[ $sched == *bfq* ]]; then
                need_fix=1
            else
                detail+="bfq is not available in this kernel. "
            fi
        fi
    done < <(lsblk -dno NAME,ROTA 2>/dev/null)
    if ((found == 0)); then
        add_finding "io-scheduler" "ok" "I/O scheduler" "No rotational disks found." "0"
    elif ((need_fix == 1)); then
        add_finding "io-scheduler" "warn" "Rotational disk is not using bfq" \
            "$detail bfq keeps interactive work responsive on a spinning disk." "1"
    else
        add_finding "io-scheduler" "ok" "I/O scheduler" "$detail" "0"
    fi
}

fix_io_scheduler() {
    local rule="/etc/udev/rules.d/60-hdd-scheduler.rules"
    local content='SUBSYSTEM=="block", KERNEL=="sd[a-z]|vd[a-z]|hd[a-z]", ATTR{queue/rotational}=="1", ATTR{queue/scheduler}="bfq"'
    local name rota sched applied=""
    local need=0
    while read -r name rota; do
        [[ ${rota:-0} == "1" ]] || continue
        sched=$(cat "/sys/block/$name/queue/scheduler" 2>/dev/null) || continue
        [[ $sched == *bfq* ]] || continue
        local active
        active=$(printf '%s' "$sched" | sed -n 's/.*\[\([^]]*\)\].*/\1/p')
        [[ $active == "bfq" ]] || need=1
    done < <(lsblk -dno NAME,ROTA 2>/dev/null)
    if ((need == 0)); then
        printf 'skipping scheduler fix: bfq is already active (or unavailable) on all rotational disks\n'
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'cat > %q <<'\''EOF'\''\n%s\nEOF\n' "$rule" "$content"
        printf 'chmod 0644 %q\n' "$rule"
        while read -r name rota; do
            [[ ${rota:-0} == "1" ]] || continue
            sched=$(cat "/sys/block/$name/queue/scheduler" 2>/dev/null) || continue
            [[ $sched == *bfq* ]] || continue
            printf 'printf %%s\\n bfq > /sys/block/%s/queue/scheduler\n' "$name"
        done < <(lsblk -dno NAME,ROTA 2>/dev/null)
        printf 'udevadm control --reload-rules\n'
        printf 'udevadm trigger --subsystem-match=block\n'
        return
    fi
    printf '%s\n' "$content" >"$rule"
    chmod 0644 "$rule"
    while read -r name rota; do
        [[ ${rota:-0} == "1" ]] || continue
        sched=$(cat "/sys/block/$name/queue/scheduler" 2>/dev/null) || continue
        [[ $sched == *bfq* ]] || continue
        printf 'bfq' >"/sys/block/$name/queue/scheduler" 2>/dev/null || true
        applied+="$name "
    done < <(lsblk -dno NAME,ROTA 2>/dev/null)
    udevadm control --reload-rules >/dev/null 2>&1 || true
    udevadm trigger --subsystem-match=block >/dev/null 2>&1 || true
    printf 'installed %s; bfq set now on: %s\n' "$rule" "${applied:-none}"
}

# ---------------------------------------------------------------------------
# 5. Junk
# ---------------------------------------------------------------------------
check_junk() {
    local journal_mb dnf_mb core_mb core_count kernel_count unused_flatpak=0
    journal_mb=$(dir_mb /var/log/journal /run/log/journal)
    if ((journal_mb > 1024)); then
        add_finding "junk-journald" "warn" "journald logs are large" \
            "$(fmt_mb "$journal_mb") of journals. Vacuuming to 200 MB is safe." "1"
    else
        add_finding "junk-journald" "ok" "journald size" "$(fmt_mb "$journal_mb")." "0"
    fi

    dnf_mb=$(dir_mb /var/cache/libdnf5 /var/cache/dnf)
    if ((dnf_mb > 500)); then
        add_finding "junk-dnf" "warn" "dnf cache is large" \
            "$(fmt_mb "$dnf_mb") of package cache. 'dnf clean all' is safe." "1"
    else
        add_finding "junk-dnf" "ok" "dnf cache" "$(fmt_mb "$dnf_mb")." "0"
    fi

    core_mb=$(dir_mb /var/lib/systemd/coredump)
    core_count=0
    if [[ -d /var/lib/systemd/coredump ]]; then
        core_count=$(find /var/lib/systemd/coredump -maxdepth 1 -type f -name 'core.*' 2>/dev/null | wc -l)
    fi
    if ((core_mb > 100 || core_count > 20)); then
        add_finding "junk-coredumps" "warn" "Many coredumps are stored" \
            "$(fmt_mb "$core_mb") in $core_count files. Old crash dumps can be removed safely." "1"
    else
        add_finding "junk-coredumps" "ok" "Coredumps" "$(fmt_mb "$core_mb") in $core_count files." "0"
    fi

    kernel_count=0
    if have rpm; then
        kernel_count=$(rpm -q kernel-core 2>/dev/null | grep -c '^kernel-core-' || true)
    fi
    if ((kernel_count > 3)); then
        add_finding "junk-kernels" "warn" "Old kernels are installed" \
            "$kernel_count kernel-core packages. Keep 2-3 and remove the rest with dnf; this is advice only." "0"
    else
        add_finding "junk-kernels" "ok" "Installed kernels" "$kernel_count kernel-core packages." "0"
    fi

    if have flatpak; then
        local installed used
        installed=$(mktemp)
        used=$(mktemp)
        flatpak list --runtime --columns=ref 2>/dev/null | sort -u >"$installed"
        flatpak list --app --columns=runtime 2>/dev/null | sort -u >"$used"
        unused_flatpak=$(comm -23 "$installed" "$used" 2>/dev/null | wc -l)
        rm -f "$installed" "$used"
        if ((unused_flatpak > 0)); then
            add_finding "junk-flatpak" "warn" "Unused Flatpak runtimes" \
                "$unused_flatpak runtime refs are not used by any app. 'flatpak uninstall --unused' is safe." "1"
        else
            add_finding "junk-flatpak" "ok" "Flatpak runtimes" "No unused runtimes detected." "0"
        fi
    else
        add_finding "junk-flatpak" "ok" "Flatpak" "Flatpak is not installed." "0"
    fi

    local cache_mb trash_mb varlog_mb
    cache_mb=$(dir_mb "$REAL_HOME/.cache")
    trash_mb=$(dir_mb "$REAL_HOME/.local/share/Trash")
    varlog_mb=$(dir_mb /var/log)
    if ((cache_mb > 2048)); then
        add_finding "junk-cache" "warn" "User cache is large" \
            "$(fmt_mb "$cache_mb") in $REAL_HOME/.cache. Review it manually; cache can be regenerated but some apps keep offline data there. No automatic fix." "0"
    else
        add_finding "junk-cache" "ok" "User cache" "$(fmt_mb "$cache_mb")." "0"
    fi
    if ((trash_mb > 1024)); then
        add_finding "junk-trash" "warn" "Trash is large" \
            "$(fmt_mb "$trash_mb") in Trash. Empty it yourself after checking what is there. No automatic fix." "0"
    else
        add_finding "junk-trash" "ok" "Trash" "$(fmt_mb "$trash_mb")." "0"
    fi
    if ((varlog_mb > 4096)); then
        add_finding "junk-varlog" "warn" "/var/log is large" \
            "$(fmt_mb "$varlog_mb"). Inspect logs; only journald is vacuumed automatically. No automatic fix." "0"
    else
        add_finding "junk-varlog" "ok" "/var/log size" "$(fmt_mb "$varlog_mb")." "0"
    fi
}

fix_junk_journald() {
    local mb
    mb=$(dir_mb /var/log/journal /run/log/journal)
    if ((mb <= 1024)); then
        printf 'skipping journald vacuum: journals are %s\n' "$(fmt_mb "$mb")"
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'journalctl --vacuum-size=200M\n'
        return
    fi
    printf 'vacuuming journald to 200 MB...\n'
    journalctl --vacuum-size=200M >/dev/null 2>&1 || printf 'journalctl vacuum failed\n'
}

fix_junk_dnf() {
    local mb
    mb=$(dir_mb /var/cache/libdnf5 /var/cache/dnf)
    if ((mb <= 500)); then
        printf 'skipping dnf clean: cache is %s\n' "$(fmt_mb "$mb")"
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'dnf clean all\n'
        return
    fi
    printf 'cleaning dnf cache...\n'
    dnf clean all >/dev/null 2>&1 || printf 'dnf clean failed\n'
}

fix_junk_coredumps() {
    local mb count
    mb=$(dir_mb /var/lib/systemd/coredump)
    count=0
    if [[ -d /var/lib/systemd/coredump ]]; then
        count=$(find /var/lib/systemd/coredump -maxdepth 1 -type f -name 'core.*' 2>/dev/null | wc -l)
    fi
    if ((mb <= 100 && count <= 20)); then
        printf 'skipping coredump cleanup: %s in %s files\n' "$(fmt_mb "$mb")" "$count"
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'find /var/lib/systemd/coredump -maxdepth 1 -type f -name '\''core.*'\'' -delete\n'
        return
    fi
    if [[ -d /var/lib/systemd/coredump ]]; then
        find /var/lib/systemd/coredump -maxdepth 1 -type f -name 'core.*' -delete 2>/dev/null || true
    fi
    printf 'removed stored coredumps\n'
}

fix_junk_flatpak() {
    if ! have flatpak; then
        printf 'flatpak is not installed; nothing to do\n'
        return
    fi
    local installed used unused
    installed=$(mktemp)
    used=$(mktemp)
    flatpak list --runtime --columns=ref 2>/dev/null | sort -u >"$installed"
    flatpak list --app --columns=runtime 2>/dev/null | sort -u >"$used"
    unused=$(comm -23 "$installed" "$used" 2>/dev/null | wc -l)
    rm -f "$installed" "$used"
    if ((unused == 0)); then
        printf 'skipping Flatpak cleanup: no unused runtimes\n'
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'flatpak repair --user\n'
        printf 'flatpak uninstall --user --unused --noninteractive -y\n'
        return
    fi
    flatpak repair --user >/dev/null 2>&1 || printf 'flatpak repair reported an error\n'
    flatpak uninstall --user --unused --noninteractive -y >/dev/null 2>&1 || printf 'flatpak uninstall reported an error\n'
    printf 'repaired user installation and removed unused runtimes\n'
}

# ---------------------------------------------------------------------------
# 6. Background services
# ---------------------------------------------------------------------------
check_services() {
    local failed failed_count blame indexers
    failed=$(systemctl --failed --no-legend --no-pager 2>/dev/null | sed '/^[[:space:]]*$/d')
    failed_count=$(printf '%s\n' "$failed" | sed '/^[[:space:]]*$/d' | wc -l)
    if ((failed_count > 0)); then
        add_finding "services-failed" "warn" "Failed systemd units" \
            "$failed_count failed unit(s): $(printf '%s' "$failed" | tr '\n' ' ')" "0"
    else
        add_finding "services-failed" "ok" "Failed systemd units" "None." "0"
    fi

    blame=$(systemd-analyze blame 2>/dev/null | head -5 | tr '\n' ';')
    add_finding "services-slowboot" "ok" "Slowest boot units" \
        "${blame:-systemd-analyze blame unavailable} (information only)." "0"

    indexers=$(ps -eo comm= 2>/dev/null | awk '$1 ~ /^tracker-/ || $1 == "baloo_file" || $1 == "balooctl" {print $1}' | sort -u | tr '\n' ' ')
    if [[ -n ${indexers// /} ]]; then
        add_finding "services-indexers" "warn" "File indexers are running" \
            "Running: $indexers These compete with the HDD at login. Masking tracker or disabling baloo is safe if you do not use file search." "1"
    else
        add_finding "services-indexers" "ok" "File indexers" "tracker and baloo are not running." "0"
    fi
}

fix_services_indexers() {
    local did=0
    local tracker_found=0
    local running
    running=$(ps -eo comm= 2>/dev/null | awk '$1 ~ /^tracker-/ || $1 == "baloo_file" || $1 == "balooctl" {print $1}' | sort -u | tr '\n' ' ')
    if [[ -z ${running// /} ]]; then
        printf 'skipping indexer fix: tracker and baloo are not running\n'
        return
    fi
    if as_real_user systemctl --user cat tracker-miner-fs-3.service >/dev/null 2>&1 ||
        as_real_user systemctl --user cat tracker-extract-3.service >/dev/null 2>&1; then
        tracker_found=1
    fi
    if ((tracker_found == 1)); then
        if ((DRY_RUN == 1)); then
            printf 'sudo -u %q env XDG_RUNTIME_DIR=%q systemctl --user mask tracker-miner-fs-3.service tracker-extract-3.service\n' "$REAL_USER" "$XDG_RUNTIME"
        else
            as_real_user systemctl --user mask tracker-miner-fs-3.service tracker-extract-3.service >/dev/null 2>&1 || true
            printf 'masked tracker user services\n'
        fi
        did=1
    fi
    if have balooctl6; then
        if ((DRY_RUN == 1)); then
            printf 'balooctl6 disable\n'
        else
            balooctl6 disable >/dev/null 2>&1 || true
            printf 'disabled baloo indexing\n'
        fi
        did=1
    elif have balooctl; then
        if ((DRY_RUN == 1)); then
            printf 'balooctl disable\n'
        else
            balooctl disable >/dev/null 2>&1 || true
            printf 'disabled baloo indexing\n'
        fi
        did=1
    fi
    if ((did == 0)); then
        printf 'tracker and baloo are not installed; nothing to mask\n'
    fi
}

# ---------------------------------------------------------------------------
# 7. Kernel tuning
# ---------------------------------------------------------------------------
check_kernel_tuning() {
    local file="/etc/sysctl.d/99-hdd.conf"
    if [[ -f $file ]]; then
        local missing=""
        grep -q '^vm.swappiness=10$' "$file" 2>/dev/null || missing+="vm.swappiness=10 "
        grep -q '^vm.dirty_background_ratio=5$' "$file" 2>/dev/null || missing+="vm.dirty_background_ratio=5 "
        grep -q '^vm.dirty_ratio=10$' "$file" 2>/dev/null || missing+="vm.dirty_ratio=10 "
        grep -q '^vm.vfs_cache_pressure=50$' "$file" 2>/dev/null || missing+="vm.vfs_cache_pressure=50 "
        if [[ -n $missing ]]; then
            add_finding "kernel-tuning" "warn" "Kernel tuning file is incomplete" \
                "$file exists but is missing: $missing Edit it yourself; the doctor will not overwrite an existing file." "0"
        else
            add_finding "kernel-tuning" "ok" "Kernel tuning" "$file contains the HDD-friendly settings." "0"
        fi
    else
        add_finding "kernel-tuning" "warn" "Kernel tuning for HDDs is not installed" \
            "$file is missing. The safe settings reduce swap and write bursts." "1"
    fi
}

fix_kernel_tuning() {
    local file="/etc/sysctl.d/99-hdd.conf"
    if [[ -f $file ]]; then
        printf '%s already exists; not overwriting\n' "$file"
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'cat > %q <<'\''EOF'\''\nvm.swappiness=10\nvm.dirty_background_ratio=5\nvm.dirty_ratio=10\nvm.vfs_cache_pressure=50\nEOF\n' "$file"
        printf 'sysctl --system\n'
        return
    fi
    cat >"$file" <<'EOF'
vm.swappiness=10
vm.dirty_background_ratio=5
vm.dirty_ratio=10
vm.vfs_cache_pressure=50
EOF
    chmod 0644 "$file"
    sysctl --system >/dev/null 2>&1 || true
    printf 'wrote %s and applied sysctl settings\n' "$file"
}

# ---------------------------------------------------------------------------
# 8. Other
# ---------------------------------------------------------------------------
check_other() {
    local avc=0 packagekit=""
    if have ausearch; then
        avc=$(ausearch -m AVC -ts recent 2>/dev/null | grep -c 'type=AVC' || true)
    else
        avc=$(journalctl --since "24 hours ago" --no-pager 2>/dev/null | grep -c 'avc:  denied' || true)
    fi
    if ((avc > 20)); then
        add_finding "other-selinux" "warn" "SELinux denials are flooding the journal" \
            "$avc AVC denials in the recent journal. Investigate with 'ausearch -m AVC -ts recent'; do not disable SELinux." "0"
    else
        add_finding "other-selinux" "ok" "SELinux denials" "$avc recent AVC denials." "0"
    fi

    packagekit=$(ps -eo comm=,pcpu= 2>/dev/null | awk '$1 ~ /^(packagekitd|dnf|dnf-makecache)$/ && $2 + 0 > 20 {print $1 " " $2 "%"}' | tr '\n' ' ')
    if [[ -n ${packagekit// /} ]]; then
        add_finding "other-packagekit" "warn" "Package tools are busy" \
            "$packagekit is using significant CPU and may be refreshing metadata. Let it finish; do not kill it mid-transaction." "0"
    else
        add_finding "other-packagekit" "ok" "Package tools" "No PackageKit/dnf process above 20% CPU." "0"
    fi

    local bigdirs
    bigdirs=$(timeout 20 du -xh --max-depth=2 / 2>/dev/null | sort -rh | head -5 | tr '\n' ';')
    add_finding "other-bigdirs" "ok" "Largest directories under /" "${bigdirs:-unavailable}" "0"

    local file_descriptor_detail
    file_descriptor_detail=$(timeout 5 cat /proc/sys/fs/file-max 2>/dev/null || echo "unavailable")
    add_finding "other-file-descriptors" "ok" "File descriptor limit" "fs.file-max: $file_descriptor_detail" "0"

    local aslr_detail
    aslr_detail=$(timeout 5 cat /proc/sys/kernel/randomize_va_space 2>/dev/null || echo "unavailable")
    add_finding "other-aslr" "ok" "ASLR setting" "kernel.randomize_va_space: $aslr_detail" "0"
}

# ---------------------------------------------------------------------------
# App startup checks
# ---------------------------------------------------------------------------
check_startup_autostart() {
    local count=0 names="" dir entry name
    for dir in "$REAL_HOME/.config/autostart" "/etc/xdg/autostart"; do
        [[ -d $dir ]] || continue
        while IFS= read -r entry; do
            count=$((count + 1))
            name=$(awk -F= '/^Name=/ {print $2; exit}' "$entry" 2>/dev/null)
            if ((${#names} < 300)); then
                names+="${name:-$(basename "$entry")}; "
            fi
        done < <(find "$dir" -maxdepth 1 -type f -name '*.desktop' 2>/dev/null | sort)
    done
    if ((count > 10)); then
        add_finding "startup-autostart" "warn" "Many autostart entries" \
            "$count autostart entries: $names Too many programs start at login and fight over the HDD. Disable the heaviest ones. Advice only." "0"
    else
        add_finding "startup-autostart" "ok" "Autostart entries" \
            "$count entries: ${names:-none}." "0"
    fi
}

check_startup_fontcache() {
    local cache_dir="$REAL_HOME/.cache/fontconfig"
    local fc_ms=0 stale=0 count=0
    if have fc-list; then
        fc_ms=$(time_ms timeout 10 fc-list)
        count=$(fc-list 2>/dev/null | wc -l)
    fi
    if [[ ! -d $cache_dir ]] || ! find "$cache_dir" -maxdepth 1 -type f -print -quit 2>/dev/null | grep -q .; then
        stale=1
    elif find /usr/share/fonts "$REAL_HOME/.local/share/fonts" "$REAL_HOME/.fonts" -type f -newer "$cache_dir" -print -quit 2>/dev/null | grep -q .; then
        stale=1
    fi
    if ((stale == 1 || fc_ms > 500)); then
        add_finding "startup-fontcache" "warn" "Font cache is stale or slow" \
            "fc-list took ${fc_ms} ms for $count fonts; stale cache=$stale. Rebuilding the cache makes app text rendering faster." "1"
    else
        add_finding "startup-fontcache" "ok" "Font cache" \
            "fc-list took ${fc_ms} ms for $count fonts; cache looks fresh." "0"
    fi
}

fix_startup_fontcache() {
    if ! have fc-cache; then
        printf 'fc-cache is not installed; nothing to do\n'
        return
    fi
    local cache_dir="$REAL_HOME/.cache/fontconfig"
    local stale=0 fc_ms
    if [[ ! -d $cache_dir ]] || ! find "$cache_dir" -maxdepth 1 -type f -print -quit 2>/dev/null | grep -q .; then
        stale=1
    elif find /usr/share/fonts "$REAL_HOME/.local/share/fonts" "$REAL_HOME/.fonts" -type f -newer "$cache_dir" -print -quit 2>/dev/null | grep -q .; then
        stale=1
    fi
    fc_ms=$(time_ms timeout 10 fc-list)
    if ((stale == 0 && fc_ms <= 500)); then
        printf 'skipping font cache: fresh and fast (%s ms)\n' "$fc_ms"
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'sudo -u %q env HOME=%q fc-cache -r\n' "$REAL_USER" "$REAL_HOME"
        return
    fi
    as_real_user fc-cache -r >/dev/null 2>&1 || printf 'fc-cache reported an error\n'
    printf 'rebuilt font cache as %s\n' "$REAL_USER"
}

check_startup_iconcache() {
    local missing="" stale=0 hicolor="/usr/share/icons/hicolor"
    local cache="$hicolor/icon-theme.cache"
    local local_apps="$REAL_HOME/.local/share/applications"
    if [[ -d $hicolor ]]; then
        if [[ ! -f $cache ]]; then
            missing+="hicolor icon cache missing; "
        elif find "$hicolor" -type f -newer "$cache" -print -quit 2>/dev/null | grep -q .; then
            stale=1
        fi
    fi
    if [[ -d $local_apps ]] && ! find "$local_apps" -maxdepth 1 -type f -name 'mimeinfo.cache' -print -quit 2>/dev/null | grep -q .; then
        missing+="desktop database cache missing; "
    fi
    if [[ -n $missing || $stale -eq 1 ]]; then
        add_finding "startup-iconcache" "warn" "Icon or desktop caches are stale" \
            "${missing}stale=$stale. Rebuilding them avoids slow icon and menu loading." "1"
    else
        add_finding "startup-iconcache" "ok" "Icon and desktop caches" "Caches look fresh." "0"
    fi
}

fix_startup_iconcache() {
    local did=0
    local hicolor="/usr/share/icons/hicolor"
    local cache="$hicolor/icon-theme.cache"
    local local_apps="$REAL_HOME/.local/share/applications"
    local needed=0
    if [[ -d $hicolor ]]; then
        if [[ ! -f $cache ]] || find "$hicolor" -type f -newer "$cache" -print -quit 2>/dev/null | grep -q .; then
            needed=1
        fi
    fi
    if [[ -d $local_apps ]] && ! find "$local_apps" -maxdepth 1 -type f -name 'mimeinfo.cache' -print -quit 2>/dev/null | grep -q .; then
        needed=1
    fi
    if ((needed == 0)); then
        printf 'skipping icon cache: caches are fresh\n'
        return
    fi
    if have gtk-update-icon-cache && [[ -d /usr/share/icons/hicolor ]]; then
        if ((DRY_RUN == 1)); then
            printf 'gtk-update-icon-cache -f -t /usr/share/icons/hicolor\n'
            did=1
        else
            gtk-update-icon-cache -f -t /usr/share/icons/hicolor >/dev/null 2>&1 || true
            did=1
        fi
    fi
    if have update-desktop-database && [[ -d $REAL_HOME/.local/share/applications ]]; then
        if ((DRY_RUN == 1)); then
            printf 'sudo -u %q env HOME=%q update-desktop-database %q\n' "$REAL_USER" "$REAL_HOME" "$REAL_HOME/.local/share/applications"
            did=1
        else
            as_real_user update-desktop-database "$REAL_HOME/.local/share/applications" >/dev/null 2>&1 || true
            did=1
        fi
    fi
    if ((DRY_RUN == 1)); then
        if ((did == 0)); then
            printf '# icon cache tools are not installed; nothing to do\n'
        fi
        return
    fi
    printf 'rebuilt icon/desktop caches\n'
}

check_startup_shadercache() {
    local mesa="$REAL_HOME/.cache/mesa_shader_cache" nv="$REAL_HOME/.nv/GLCache"
    local mesa_mb nv_mb total disabled=0 detail
    mesa_mb=$(dir_mb "$mesa")
    nv_mb=$(dir_mb "$nv")
    total=$((mesa_mb + nv_mb))
    if [[ ${MESA_SHADER_CACHE_DISABLE:-} == "true" || ${MESA_SHADER_CACHE_DISABLE:-} == "1" ]]; then
        disabled=1
    fi
    if [[ ${__GL_SHADER_DISK_CACHE:-} == "0" || ${__GL_SHADER_DISK_CACHE:-} == "false" ]]; then
        disabled=1
    fi
    detail="mesa shader cache $(fmt_mb "$mesa_mb"), NVIDIA GLCache $(fmt_mb "$nv_mb"), disabled=$disabled."
    if ((disabled == 1)); then
        add_finding "startup-shadercache" "warn" "GPU shader cache is disabled" \
            "$detail Re-enabling disk shader caches avoids recompiling shaders on every launch. Advice only." "0"
    elif ((total == 0)); then
        add_finding "startup-shadercache" "ok" "GPU shader cache" \
            "$detail No cache yet; the first launches may compile shaders, then it should stay warm." "0"
    elif ((total > 5120)); then
        add_finding "startup-shadercache" "warn" "GPU shader cache is very large" \
            "$detail An oversized cache can slow startup; clearing old shader caches is safe but will cause a one-time recompile. Advice only." "0"
    else
        add_finding "startup-shadercache" "ok" "GPU shader cache" "$detail" "0"
    fi
}

check_startup_flatpak() {
    if ! have flatpak; then
        add_finding "startup-flatpak" "ok" "Flatpak state" "Flatpak is not installed." "0"
        return
    fi
    local broken=0 unused=0 installed used
    installed=$(mktemp)
    used=$(mktemp)
    if ! flatpak list --runtime --columns=ref >"$installed" 2>/dev/null; then
        broken=1
    fi
    flatpak list --app --columns=runtime 2>/dev/null | sort -u >"$used"
    if [[ -s $installed ]]; then
        unused=$(comm -23 <(sort -u "$installed") "$used" 2>/dev/null | wc -l)
    fi
    rm -f "$installed" "$used"
    if ((broken == 1 || unused > 0)); then
        add_finding "startup-flatpak" "warn" "Flatpak state needs attention" \
            "broken=$broken, unused runtime refs=$unused. Repairing and removing unused runtimes is safe." "1"
    else
        add_finding "startup-flatpak" "ok" "Flatpak state" "No unused runtimes and no obvious corruption." "0"
    fi
}

fix_startup_flatpak() {
    if ! have flatpak; then
        printf 'flatpak is not installed; nothing to do\n'
        return
    fi
    local installed used unused broken=0
    installed=$(mktemp)
    used=$(mktemp)
    if ! flatpak list --runtime --columns=ref >"$installed" 2>/dev/null; then
        broken=1
    fi
    flatpak list --app --columns=runtime 2>/dev/null | sort -u >"$used"
    unused=0
    if [[ -s $installed ]]; then
        unused=$(comm -23 <(sort -u "$installed") "$used" 2>/dev/null | wc -l)
    fi
    rm -f "$installed" "$used"
    if ((broken == 0 && unused == 0)); then
        printf 'skipping Flatpak repair: state looks clean\n'
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'flatpak repair --user\n'
        printf 'flatpak uninstall --user --unused --noninteractive -y\n'
        return
    fi
    flatpak repair --user >/dev/null 2>&1 || printf 'flatpak repair reported an error\n'
    flatpak uninstall --user --unused --noninteractive -y >/dev/null 2>&1 || printf 'flatpak uninstall reported an error\n'
    printf 'repaired Flatpak user installation and removed unused runtimes\n'
}

check_startup_dns() {
    local ext_ms local_ms=0 hostname_value bad=0 detail
    ext_ms=$(time_ms timeout 3 getent hosts example.com)
    hostname_value=$(hostname 2>/dev/null || printf 'localhost')
    if [[ -n $hostname_value ]]; then
        local_ms=$(time_ms timeout 3 getent hosts "$hostname_value")
    fi
    detail="example.com ${ext_ms} ms, local hostname ${local_ms} ms."
    if ((ext_ms > 500 || local_ms > 500)); then
        bad=1
    fi
    if have resolvectl; then
        local status
        status=$(timeout 3 resolvectl status 2>/dev/null | awk '/Current DNS Server:|DNS Servers:|Link [0-9]+ \(/ {print}' | head -20 | tr '\n' ' ')
        detail+=" resolver: ${status:-unavailable}"
        if printf '%s' "$status" | grep -qi 'no dns servers\|not reachable'; then
            bad=1
        fi
    fi
    if ((bad == 1)); then
        add_finding "startup-dns" "warn" "DNS lookups are slow or unhealthy" \
            "$detail Apps often block on DNS during launch. Fix /etc/resolv.conf or your NetworkManager DNS settings. Advice only." "0"
    else
        add_finding "startup-dns" "ok" "DNS latency" "$detail" "0"
    fi
}

check_startup_browser() {
    local total=0 path mb big="" found=0
    for path in "$REAL_HOME/.mozilla/firefox" "$REAL_HOME/.cache/mozilla" \
        "$REAL_HOME/.config/google-chrome" "$REAL_HOME/.cache/google-chrome" \
        "$REAL_HOME/.config/chromium" "$REAL_HOME/.cache/chromium"; do
        [[ -d $path ]] || continue
        found=1
        mb=$(dir_mb "$path")
        total=$((total + mb))
    done
    if ((found == 1)); then
        while IFS= read -r path; do
            big+="$path; "
        done < <(find "$REAL_HOME/.mozilla" "$REAL_HOME/.config/google-chrome" \
            "$REAL_HOME/.config/chromium" -type f \( -name '*.sqlite' -o -name 'places.sqlite' \
            -o -name 'History' -o -name 'Cookies' -o -name 'favicons.sqlite' \) \
            -size +200M 2>/dev/null | head -10)
    fi
    if ((total > 5120)) || [[ -n $big ]]; then
        add_finding "startup-browser-profile" "warn" "Browser profile is large" \
            "$(fmt_mb "$total") total; SQLite files over 200 MB: ${big:-none}. Consider a fresh profile or profile maintenance. Never delete a profile automatically." "0"
    elif ((found == 0)); then
        add_finding "startup-browser-profile" "ok" "Browser profile" "No Firefox/Chromium profile found for $REAL_USER." "0"
    else
        add_finding "startup-browser-profile" "ok" "Browser profile" "$(fmt_mb "$total") total; no SQLite file over 200 MB." "0"
    fi
}

check_startup_ldconfig() {
    local cache="/etc/ld.so.cache" stale=0 newer
    if [[ ! -f $cache ]]; then
        stale=1
    else
        newer=$(timeout 15 find /usr/lib64 /usr/local/lib /lib64 -type f -newer "$cache" -print -quit 2>/dev/null)
        [[ -n $newer ]] && stale=1
    fi
    if ((stale == 1)); then
        add_finding "startup-ldconfig" "warn" "Shared-library cache is stale" \
            "ld.so.cache is missing or older than installed libraries; launches may scan library directories. Running ldconfig is safe." "1"
    else
        add_finding "startup-ldconfig" "ok" "Shared-library cache" "ld.so.cache is present and up to date." "0"
    fi
}

fix_startup_ldconfig() {
    local cache="/etc/ld.so.cache"
    local stale=0 newer
    if [[ ! -f $cache ]]; then
        stale=1
    else
        newer=$(timeout 15 find /usr/lib64 /usr/local/lib /lib64 -type f -newer "$cache" -print -quit 2>/dev/null)
        [[ -n $newer ]] && stale=1
    fi
    if ((stale == 0)); then
        printf 'skipping ldconfig: cache is up to date\n'
        return
    fi
    if ((DRY_RUN == 1)); then
        printf 'ldconfig\n'
        return
    fi
    ldconfig >/dev/null 2>&1 || printf 'ldconfig reported an error\n'
    printf 'rebuilt shared-library cache\n'
}

check_startup_dbus_keyring() {
    local bus_ms secret_ms detail
    bus_ms=$(time_ms timeout 3 as_real_user busctl --user status)
    secret_ms=$(time_ms timeout 3 as_real_user busctl --user call org.freedesktop.secrets \
        /org/freedesktop/secrets org.freedesktop.DBus.Peer Ping)
    detail="session bus ${bus_ms} ms, secret service ${secret_ms} ms."
    if ((bus_ms > 500 || secret_ms > 500)); then
        add_finding "startup-dbus-keyring" "warn" "D-Bus or keyring is slow" \
            "$detail Apps often wait on the keyring or session bus at launch. Check gnome-keyring/KWallet and unlock it. Advice only." "0"
    else
        add_finding "startup-dbus-keyring" "ok" "D-Bus and keyring latency" "$detail" "0"
    fi
}

# ---------------------------------------------------------------------------
# Check driver
# ---------------------------------------------------------------------------
run_checks() {
    check_disk_health
    check_filesystem
    check_memory
    check_scheduler
    check_junk
    check_services
    check_kernel_tuning
    check_other
    check_startup_autostart
    check_startup_fontcache
    check_startup_iconcache
    check_startup_shadercache
    check_startup_flatpak
    check_startup_dns
    check_startup_browser
    check_startup_ldconfig
    check_startup_dbus_keyring
}

# ---------------------------------------------------------------------------
# Fixes
# ---------------------------------------------------------------------------
is_fix_id() {
    case "$1" in
        filesystem-btrfs-space | io-scheduler | junk-journald | junk-dnf | \
            junk-coredumps | junk-flatpak | services-indexers | kernel-tuning | \
            startup-fontcache | startup-iconcache | startup-flatpak | startup-ldconfig)
            return 0
            ;;
    esac
    return 1
}

all_fix_ids() {
    printf '%s\n' \
        filesystem-btrfs-space io-scheduler junk-journald junk-dnf \
        junk-coredumps junk-flatpak services-indexers kernel-tuning \
        startup-fontcache startup-iconcache startup-flatpak startup-ldconfig
}

run_fix() {
    local id=$1
    if ! is_fix_id "$id"; then
        printf '%s: unknown fix id: %s\n' "$PROG" "$id" >&2
        printf 'Known fixes: %s\n' "$(all_fix_ids | tr '\n' ' ')" >&2
        return 2
    fi
    if ((DRY_RUN == 0 && EUID != 0)); then
        printf '%s: fix %s requires root\n' "$PROG" "$id" >&2
        return 1
    fi
    case "$id" in
        filesystem-btrfs-space) fix_filesystem_btrfs_space ;;
        io-scheduler) fix_io_scheduler ;;
        junk-journald) fix_junk_journald ;;
        junk-dnf) fix_junk_dnf ;;
        junk-coredumps) fix_junk_coredumps ;;
        junk-flatpak) fix_junk_flatpak ;;
        services-indexers) fix_services_indexers ;;
        kernel-tuning) fix_kernel_tuning ;;
        startup-fontcache) fix_startup_fontcache ;;
        startup-iconcache) fix_startup_iconcache ;;
        startup-flatpak) fix_startup_flatpak ;;
        startup-ldconfig) fix_startup_ldconfig ;;
    esac
}

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------
print_human() {
    local i sev color label
    printf 'Fedora doctor %s — %s\n\n' "$VERSION" "$([[ $DRY_RUN -eq 1 ]] && printf 'dry run' || printf 'report only (no changes)')"
    for i in "${!FIND_IDS[@]}"; do
        sev=${FIND_SEV[$i]}
        case "$sev" in
            ok) color=$C_OK label="OK  " ;;
            warn) color=$C_WARN label="WARN" ;;
            bad) color=$C_BAD label="BAD " ;;
            *) color="" label="?   " ;;
        esac
        printf '%s[%s]%s %s: %s\n' "$color" "$label" "$C_OFF" "${FIND_IDS[$i]}" "${FIND_TITLE[$i]}"
        printf '       %s\n' "${FIND_DETAIL[$i]}"
        if [[ ${FIND_FIX[$i]} == "1" ]]; then
            printf '       %sfix: sudo %s --fix-id %s%s\n' "$C_DIM" "$PROG" "${FIND_IDS[$i]}" "$C_OFF"
        fi
    done
    local ok=0 warn=0 bad=0
    for i in "${!FIND_SEV[@]}"; do
        case "${FIND_SEV[$i]}" in
            ok) ok=$((ok + 1)) ;;
            warn) warn=$((warn + 1)) ;;
            bad) bad=$((bad + 1)) ;;
        esac
    done
    printf '\nSummary: %d OK, %d WARN, %d BAD. Report-only; nothing was changed.\n' "$ok" "$warn" "$bad"
}

if [[ $MODE == "fix" ]]; then
    if [[ -n $FIX_ID ]]; then
        run_fix "$FIX_ID"
        exit $?
    fi
    if ((DRY_RUN == 1)); then
        printf '# Commands that --fix would run (guards are evaluated now):\n'
    fi
    rc=0
    while IFS= read -r id; do
        if ((DRY_RUN == 1)); then
            printf '\n# fix %s\n' "$id"
        else
            printf '\n== %s ==\n' "$id"
        fi
        run_fix "$id" || rc=1
    done < <(all_fix_ids)
    exit "$rc"
fi

run_checks

if ((JSON == 1)); then
    emit_json
else
    print_human
fi
