#!/usr/bin/env bash
# setup-host.sh: one-time host preparation for lss on Ubuntu 24.04 / 26.04.
#
# Idempotent: every step checks or overwrites to a known state, so re-running it
# is always safe. Run as root on the box:   sudo bash deploy/setup-host.sh
#
# What it does:
#   1. kernel network tuning        /etc/sysctl.d/99-lss.conf
#   2. file descriptor limits       /etc/security/limits.d/99-lss.conf
#   3. system user `lss` and the /opt/lss, /var/lib/lss directories
#   4. journald size cap            /etc/systemd/journald.conf.d/99-lss-size.conf
#   5. disables apt timers + unattended-upgrades (no surprise CPU/disk use, and
#      no package changes during the contest)
#   6. installs curl, jq, sqlite3 if missing and the internet is reachable
#   7. installs + enables the systemd unit (starts it only if a release exists)
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "setup-host.sh: run as root (sudo bash $0)" >&2
    exit 2
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
log() { printf '\n==> %s\n' "$*"; }

# ---- 1. sysctl ------------------------------------------------------------------
log "sysctl: /etc/sysctl.d/99-lss.conf"
cat > /etc/sysctl.d/99-lss.conf <<'CONF'
# lss: tuned for up to 5,000 keep-alive connections from one load-generator IP.
# Accept queue: lets the 65535 listen backlog actually take effect.
net.core.somaxconn = 65535
# Half-open (SYN) queue for connection bursts at the start of each level.
net.ipv4.tcp_max_syn_backlog = 65535
# Packets queued between the NIC and the kernel when the CPU is busy.
net.core.netdev_max_backlog = 16384
# More ephemeral ports (matters for local load tests against localhost).
net.ipv4.ip_local_port_range = 1024 65535
# Reuse TIME_WAIT sockets for new outgoing connections (local load tests).
net.ipv4.tcp_tw_reuse = 1
# Release closed connections faster.
net.ipv4.tcp_fin_timeout = 15
# Keep the congestion window on idle keep-alive connections, so the first
# response after a pause isn't throttled.
net.ipv4.tcp_slow_start_after_idle = 0
CONF
sysctl --quiet --system

# ---- 2. file limits -----------------------------------------------------------------
# Applies to login sessions (e.g. running a load tool as ubuntu). The service
# itself gets its limit from LimitNOFILE in lss.service; PAM limits don't apply
# to systemd services.
log "limits: /etc/security/limits.d/99-lss.conf"
cat > /etc/security/limits.d/99-lss.conf <<'CONF'
*    soft nofile 65535
*    hard nofile 524288
root soft nofile 65535
root hard nofile 524288
CONF

# ---- 3. user and directories ------------------------------------------------------
log "user lss + directories"
if ! id -u lss >/dev/null 2>&1; then
    useradd --system --home-dir /var/lib/lss --no-create-home --shell /usr/sbin/nologin lss
fi
# Releases are owned by root (lss can read and run them): the service can't modify
# its own binary.
install -d -m 0755 -o root -g root /opt/lss /opt/lss/releases
# The database directory is the only place lss can write.
install -d -m 0750 -o lss -g lss /var/lib/lss

# ---- 4. journald cap ------------------------------------------------------------------
log "journald: cap at 200 MB"
install -d -m 0755 /etc/systemd/journald.conf.d
if [[ -f "$HERE/journald-size.conf" ]]; then
    install -m 0644 "$HERE/journald-size.conf" /etc/systemd/journald.conf.d/99-lss-size.conf
else
    printf '[Journal]\nSystemMaxUse=200M\n' > /etc/systemd/journald.conf.d/99-lss-size.conf
fi
systemctl restart systemd-journald

# ---- 5. no background package activity ------------------------------------------------
# apt-daily can wake up mid-run, take a CPU and the disk for minutes, and even
# restart services after an upgrade. None of that is acceptable on test day.
log "disable apt timers and unattended-upgrades"
for unit in apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service; do
    if systemctl list-unit-files "$unit" >/dev/null 2>&1; then
        systemctl disable --now "$unit" >/dev/null 2>&1 || true
        systemctl mask "$unit" >/dev/null 2>&1 || true
    fi
done
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'CONF'
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
CONF

# ---- 6. tools used by the scripts -------------------------------------------------------
# smoke.sh needs curl + jq; sqlite3 is for inspecting the DB when debugging.
missing=()
for pkg in curl jq sqlite3; do
    command -v "$pkg" >/dev/null 2>&1 || missing+=("$pkg")
done
if ((${#missing[@]})); then
    log "installing: ${missing[*]}"
    if ! { apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${missing[@]}"; }; then
        echo "WARNING: could not install ${missing[*]} (no internet?). smoke.sh needs curl and jq." >&2
    fi
fi

# ---- 7. systemd unit -------------------------------------------------------------------------
log "systemd unit"
if [[ -f "$HERE/lss.service" ]]; then
    if ! cmp -s "$HERE/lss.service" /etc/systemd/system/lss.service; then
        install -m 0644 "$HERE/lss.service" /etc/systemd/system/lss.service
        systemctl daemon-reload
    fi
fi
if [[ -f /etc/systemd/system/lss.service ]]; then
    # enable = start on every boot (WantedBy=multi-user.target).
    systemctl enable lss.service --quiet
    if [[ -x /opt/lss/current/lss-server ]]; then
        systemctl start lss.service
    else
        echo "No release installed yet; the first deploy will start the service."
    fi
else
    echo "WARNING: lss.service not found next to this script; unit not installed." >&2
fi

log "done"
echo "somaxconn=$(sysctl -n net.core.somaxconn)  lss service enabled: $(systemctl is-enabled lss.service 2>/dev/null || echo no)"
