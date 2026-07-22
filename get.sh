#!/bin/sh
# Garmin MCP — one-line installer (Linux & macOS).
#   Personal machine : curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh
#   Home server (LAN): curl -fsSL … | sh -s -- --server
#   VPS / Tailscale  : curl -fsSL … | sh -s -- --server --tailscale
#   Stable HTTPS URL : curl -fsSL … | sh -s -- --funnel   (Tailscale Funnel,
#                      public https://<machine>.<tailnet>.ts.net for claude.ai)
#   --no-sudo        : never invoke sudo/doas (privileged steps are skipped
#                      with a hint; unnecessary when already root)
# Installs everything under your user account, starts the connector at boot,
# and gives you the sign-in page. Your Garmin credentials never leave the machine.
set -e

MODE="local"
TS_ONLY=""
NO_SUDO=""
FUNNEL=""
for arg in "$@"; do
    case "$arg" in
        --server)    MODE="server" ;;
        --tailscale) MODE="server"; TS_ONLY="yes" ;;
        --funnel)    MODE="server"; FUNNEL="yes" ;;
        --no-sudo)   NO_SUDO="yes" ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done
# Funnel proxies through localhost, so it needs the connector on 0.0.0.0.
[ -n "$FUNNEL" ] && TS_ONLY=""

REPO_TARBALL="https://github.com/devfrp/mcp-garmin-for-ia/archive/refs/heads/main.tar.gz"
REPO_GIT="https://github.com/devfrp/mcp-garmin-for-ia.git"
SRC="${GARMIN_MCP_SRC:-}"
APP_DIR="${HOME}/.local/share/garmin-mcp"
BIN_DIR="${HOME}/.local/bin"
VENV="${APP_DIR}/venv"
PORT="${GARMIN_MCP_PORT:-8765}"
OS="$(uname -s)"

say() { printf '\033[1;32m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# Privilege helper: root (Proxmox LXC, VPS) needs no sudo; otherwise use
# sudo/doas when available. $ROOTDO stays empty as root or with --no-sudo
# (privileged steps are then skipped with a hint instead of attempted).
ROOTDO=""
IS_ROOT=""
if [ "$(id -u)" = "0" ]; then
    IS_ROOT="yes"
elif [ -n "$NO_SUDO" ]; then
    :
elif command -v sudo >/dev/null 2>&1; then
    ROOTDO="sudo"
elif command -v doas >/dev/null 2>&1; then
    ROOTDO="doas"
fi

tailscale_ip() { command -v tailscale >/dev/null 2>&1 && tailscale ip -4 2>/dev/null | head -n1; }

ensure_tailscale() {
    if [ "$OS" = "Linux" ] && [ ! -e /dev/net/tun ]; then
        echo "Tailscale needs /dev/net/tun, which this container does not have." >&2
        echo "Proxmox LXC: on the HOST, add to /etc/pve/lxc/<CTID>.conf then restart the CT:" >&2
        echo "  lxc.cgroup2.devices.allow: c 10:200 rwm" >&2
        echo "  lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file" >&2
        die "/dev/net/tun missing"
    fi
    if ! command -v tailscale >/dev/null 2>&1; then
        say "Installing Tailscale"
        case "$OS" in
            Darwin)
                command -v brew >/dev/null 2>&1 \
                    || die "Homebrew not found — install Tailscale from https://tailscale.com/download then re-run"
                brew install tailscale || die "Tailscale install failed"
                $ROOTDO tailscaled install-system-daemon 2>/dev/null || true
                ;;
            *)
                curl -fsSL https://tailscale.com/install.sh | sh \
                    || die "Tailscale install failed (see https://tailscale.com/download)"
                ;;
        esac
    fi
    if [ -z "$(tailscale_ip)" ]; then
        command -v systemctl >/dev/null 2>&1 \
            && $ROOTDO systemctl enable --now tailscaled 2>/dev/null || true
        say "Connecting this machine to your tailnet — sign in with the URL below if asked"
        $ROOTDO tailscale up \
            || die "could not join the tailnet — run 'tailscale up' as root, then re-run this script"
    fi
    [ -n "$(tailscale_ip)" ] || die "no Tailscale IP after setup (check: tailscale status)"
}
lan_ip() {
    case "$OS" in
        Darwin) ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null ;;
        *)      ip route get 1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n1 ;;
    esac
}

# ── Bind address ──
BIND="127.0.0.1"
if [ "$MODE" = "server" ]; then
    if [ -n "$TS_ONLY" ]; then
        ensure_tailscale
        BIND="$(tailscale_ip)"
    else
        [ -n "$FUNNEL" ] && ensure_tailscale
        BIND="0.0.0.0"
    fi
fi
CHECK_HOST="$BIND"
[ "$BIND" = "0.0.0.0" ] && CHECK_HOST="127.0.0.1"
LINK="http://${CHECK_HOST}:${PORT}/setup"

# ── 1. Python ≥ 3.10 ──
PY=""
for c in python3 python; do
    command -v "$c" >/dev/null 2>&1 && { PY="$c"; break; }
done
[ -n "$PY" ] || {
    echo "Python 3.10+ is required. Install it first:" >&2
    echo "  Arch          : sudo pacman -S python" >&2
    echo "  Debian/Ubuntu : sudo apt install python3 python3-venv" >&2
    echo "  Fedora        : sudo dnf install python3" >&2
    echo "  macOS         : brew install python" >&2
    die "python3 not found"
}
"$PY" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)' \
    || die "Python 3.10+ required (found $($PY --version 2>&1))"

# ── 2. Install the connector ──
say "Installing the Garmin MCP connector (user-only, ${APP_DIR})"
mkdir -p "$APP_DIR" "$BIN_DIR"
"$PY" -m venv "$VENV" || die "could not create a virtualenv (python3-venv missing?)"
"$VENV/bin/pip" install --quiet --upgrade pip
if [ -n "$SRC" ]; then
    "$VENV/bin/pip" install --quiet --upgrade "$SRC" || die "pip install failed ($SRC)"
elif "$VENV/bin/pip" install --quiet --upgrade \
        "garmin-mcp-connector @ ${REPO_TARBALL}#subdirectory=connector" 2>/dev/null; then
    :
else
    command -v git >/dev/null 2>&1 || die "pip install failed and git is not available"
    "$VENV/bin/pip" install --quiet --upgrade \
        "git+${REPO_GIT}#subdirectory=connector" || die "pip install failed"
fi
ln -sf "$VENV/bin/garmin-mcp" "$BIN_DIR/garmin-mcp"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: add ${BIN_DIR} to your PATH to use the 'garmin-mcp' command." ;;
esac

# ── 3. cloudflared (optional — HTTPS link for claude.ai) ──
if ! command -v cloudflared >/dev/null 2>&1; then
    echo "note: for claude.ai in the browser, install cloudflared later:"
    case "$OS" in
        Darwin) echo "        brew install cloudflared" ;;
        *)      echo "        Arch: pacman -S cloudflared"
                echo "        Debian/Ubuntu: curl -fsSL -o /tmp/cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb && apt install -y /tmp/cloudflared.deb" ;;
    esac
fi

# ── 4. Start the connector (and keep it started across reboots) ──
STARTED=""
if [ "$OS" = "Linux" ] && [ -n "$IS_ROOT" ] && command -v systemctl >/dev/null 2>&1; then
    # Running as root (Proxmox LXC, VPS): system-wide unit, boots on its own,
    # no sudo and no lingering involved.
    cat > /etc/systemd/system/garmin-mcp.service <<EOF
[Unit]
Description=Garmin MCP local connector
After=network-online.target tailscaled.service
Wants=network-online.target

[Service]
ExecStart=${VENV}/bin/garmin-mcp serve --host ${BIND} --port ${PORT}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    if systemctl daemon-reload && systemctl enable garmin-mcp.service >/dev/null 2>&1 \
            && systemctl restart garmin-mcp.service; then
        STARTED="systemd-system"
    fi
elif [ "$OS" = "Linux" ] && command -v systemctl >/dev/null 2>&1 \
        && [ -d "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" ]; then
    UNIT_DIR="${HOME}/.config/systemd/user"
    mkdir -p "$UNIT_DIR"
    cat > "${UNIT_DIR}/garmin-mcp.service" <<EOF
[Unit]
Description=Garmin MCP local connector

[Service]
ExecStart=${VENV}/bin/garmin-mcp serve --host ${BIND} --port ${PORT}
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
EOF
    if systemctl --user daemon-reload 2>/dev/null \
            && systemctl --user enable garmin-mcp.service 2>/dev/null \
            && systemctl --user restart garmin-mcp.service 2>/dev/null; then
        STARTED="systemd"
    fi
elif [ "$OS" = "Darwin" ]; then
    PLIST="${HOME}/Library/LaunchAgents/com.garmin-mcp.connector.plist"
    mkdir -p "${HOME}/Library/LaunchAgents"
    cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
 "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.garmin-mcp.connector</string>
  <key>ProgramArguments</key>
  <array><string>${VENV}/bin/garmin-mcp</string><string>serve</string>
         <string>--host</string><string>${BIND}</string>
         <string>--port</string><string>${PORT}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
</dict></plist>
EOF
    launchctl unload "$PLIST" 2>/dev/null || true
    launchctl load "$PLIST" 2>/dev/null && STARTED="launchd"
fi
if [ -z "$STARTED" ]; then
    if ! curl -fsS -m 2 "http://${CHECK_HOST}:${PORT}/health" >/dev/null 2>&1; then
        nohup "$VENV/bin/garmin-mcp" serve --host "$BIND" --port "$PORT" \
            > "${APP_DIR}/connector.log" 2>&1 &
        STARTED="background"
    else
        STARTED="already-running"
    fi
fi

# ── 5. Boot persistence (user services need lingering; system units do not) ──
if [ "$MODE" = "server" ] && [ "$STARTED" = "systemd" ]; then
    if ! loginctl show-user "$USER" --property=Linger 2>/dev/null | grep -q "=yes"; then
        if [ -n "$ROOTDO" ] && $ROOTDO loginctl enable-linger "$USER" 2>/dev/null; then
            say "Start-at-boot enabled."
        else
            echo ""
            echo "To start the connector at boot, run once (as root):"
            echo "  loginctl enable-linger $USER"
            echo ""
        fi
    fi
fi

# ── 6. Wait until it answers, then hand over the links ──
i=0
while [ $i -lt 20 ]; do
    curl -fsS -m 1 "http://${CHECK_HOST}:${PORT}/health" >/dev/null 2>&1 && break
    i=$((i + 1)); sleep 0.5
done
curl -fsS -m 2 "http://${CHECK_HOST}:${PORT}/health" >/dev/null 2>&1 \
    || die "the connector did not start (try: ${VENV}/bin/garmin-mcp serve --host ${BIND})"

# ── 7. Stable public HTTPS URL via Tailscale Funnel (--funnel) ──
FUNNEL_URL=""
if [ -n "$FUNNEL" ]; then
    say "Enabling the stable HTTPS address (Tailscale Funnel)"
    if ! $ROOTDO tailscale funnel --bg "$PORT"; then
        echo "If a link to enable Funnel for your tailnet was shown above, open it," >&2
        die "then re-run this script (tailscale funnel failed)"
    fi
    TSDNS="$($ROOTDO tailscale status --json 2>/dev/null \
        | "$PY" -c "import json,sys; print(json.load(sys.stdin)['Self']['DNSName'].rstrip('.'))" 2>/dev/null)"
    TOKEN="$(cat "${GARMIN_MCP_HOME:-$HOME/.config/garmin-mcp}/access_token" 2>/dev/null)"
    if [ -n "$TSDNS" ]; then
        FUNNEL_URL="https://${TSDNS}/garmin/?token=${TOKEN:-<run: garmin-mcp url>}"
    fi
fi

say "Connector running (${STARTED}, listening on ${BIND}:${PORT})."
echo ""
echo "  Sign in to Garmin here:"
if [ "$BIND" = "0.0.0.0" ]; then
    echo "    this machine   : http://127.0.0.1:${PORT}/setup"
    LAN="$(lan_ip)";       [ -n "$LAN" ] && echo "    from your LAN  : http://${LAN}:${PORT}/setup"
    TS="$(tailscale_ip)";  [ -n "$TS" ]  && echo "    via Tailscale  : http://${TS}:${PORT}/setup"
else
    echo "    ${LINK}"
fi
echo ""
if [ -n "$FUNNEL_URL" ]; then
    echo "  Stable HTTPS MCP URL for claude.ai (survives reboots):"
    echo "    ${FUNNEL_URL}"
    echo ""
fi
echo "Your email and password go from your browser to this machine to Garmin —"
echo "no third-party server ever sees them."
if [ "$MODE" = "server" ] && [ "$BIND" = "0.0.0.0" ] && [ -z "$FUNNEL" ]; then
    echo ""
    echo "warning: listening on all interfaces. Fine on a home LAN; on a VPS with a"
    echo "public IP, reinstall with:  sh get.sh --server --tailscale"
fi
if [ "$MODE" = "local" ]; then
    case "$OS" in
        Darwin) open "$LINK" >/dev/null 2>&1 || true ;;
        *)      command -v xdg-open >/dev/null 2>&1 && xdg-open "$LINK" >/dev/null 2>&1 || true ;;
    esac
fi
