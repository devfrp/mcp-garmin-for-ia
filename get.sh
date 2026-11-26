#!/bin/sh
# Garmin MCP — one-line installer (Linux & macOS).
#   Personal machine  : curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh
#   Home server (LAN) : curl -fsSL … | sh -s -- --server
#   VPS / Tailscale   : curl -fsSL … | sh -s -- --server --tailscale
#   VPS / NetBird     : curl -fsSL … | sh -s -- --server --netbird
#       (self-hosted NetBird or unattended join: add
#        --netbird-management-url=https://netbird.example.com and/or
#        NETBIRD_SETUP_KEY=… in the environment)
#   Stable HTTPS URL, private to your tailnet only (no public exposure) — for
#   a client you run yourself on a tailnet machine (e.g. Claude Code). Does
#   NOT work for claude.ai's/Desktop's "Add custom connector" (see --funnel):
#                       curl -fsSL … | sh -s -- --tailscale-serve
#   Stable HTTPS URL, reachable from the public Internet (Tailscale Funnel) —
#   required for claude.ai's/Desktop's "Add custom connector" (see the
#   warning this prints):
#                       curl -fsSL … | sh -s -- --funnel
#   --no-sudo         : never invoke sudo/doas (privileged steps are skipped
#                       with a hint; unnecessary when already root)
# Installs a single self-contained binary under your user account (no Python,
# no runtime), starts the connector at boot, and gives you the sign-in page.
# Your Garmin credentials never leave the machine.
# Safe to re-run any time (e.g. to switch flags or update): it never deletes
# your Garmin login or logs Tailscale/NetBird out, it only reinstalls the
# connector binary and restarts the service.
set -e

MODE="local"
TS_ONLY=""
NB_ONLY=""
NO_SUDO=""
FUNNEL=""
TS_SERVE=""
NB_SETUP_KEY="${NETBIRD_SETUP_KEY:-}"
NB_MGMT_URL="${NETBIRD_MANAGEMENT_URL:-}"
for arg in "$@"; do
    case "$arg" in
        --server)          MODE="server" ;;
        --tailscale)       MODE="server"; TS_ONLY="yes" ;;
        --tailscale-serve) MODE="server"; TS_SERVE="yes" ;;
        --funnel)          MODE="server"; FUNNEL="yes" ;;
        --netbird)         MODE="server"; NB_ONLY="yes" ;;
        --netbird-setup-key=*)      MODE="server"; NB_ONLY="yes"; NB_SETUP_KEY="${arg#*=}" ;;
        --netbird-management-url=*) MODE="server"; NB_ONLY="yes"; NB_MGMT_URL="${arg#*=}" ;;
        --no-sudo)   NO_SUDO="yes" ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

REPO="devfrp/mcp-garmin-for-ia"
REPO_GIT="https://github.com/${REPO}.git"
RELEASE_URL="${GARMIN_MCP_RELEASE_URL:-https://github.com/${REPO}/releases/latest/download}"
SRC="${GARMIN_MCP_SRC:-}"
APP_DIR="${HOME}/.local/share/garmin-mcp"
BIN_DIR="${HOME}/.local/bin"
EXE="${APP_DIR}/bin/garmin-mcp"
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

# Only while Tailscale is up: `tailscale ip -4` still answers when it is stopped.
tailscale_ip() {
    command -v tailscale >/dev/null 2>&1 || return 0
    tailscale status --json 2>/dev/null | tr ',' '\n' | grep -q '"BackendState": *"Running"' \
        || return 0
    tailscale ip -4 2>/dev/null | head -n1
}

# NetBird overlay IPv4 of this peer (`netbird status --ipv4`), or the wt0
# interface when the daemon socket isn't readable by this user.
netbird_ip() {
    command -v netbird >/dev/null 2>&1 || return 0
    _nb="$(netbird status --ipv4 2>/dev/null | sed -n 's/^\([0-9][0-9.]*\).*/\1/p' | head -n1)"
    if [ -z "$_nb" ] && [ "$OS" = "Linux" ] && command -v ip >/dev/null 2>&1; then
        _nb="$(ip -4 -o addr show dev wt0 2>/dev/null | sed -n 's/.* inet \([0-9.]*\).*/\1/p' | head -n1)"
    fi
    [ -n "$_nb" ] && echo "$_nb"
    return 0
}

need_tun() {
    if [ "$OS" = "Linux" ] && [ ! -e /dev/net/tun ]; then
        echo "$1 needs /dev/net/tun, which this container does not have." >&2
        echo "Proxmox LXC: on the HOST, add to /etc/pve/lxc/<CTID>.conf then restart the CT:" >&2
        echo "  lxc.cgroup2.devices.allow: c 10:200 rwm" >&2
        echo "  lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file" >&2
        die "/dev/net/tun missing"
    fi
}

ensure_tailscale() {
    need_tun Tailscale
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

ensure_netbird() {
    need_tun NetBird
    if ! command -v netbird >/dev/null 2>&1; then
        say "Installing NetBird"
        curl -fsSL https://pkgs.netbird.io/install.sh | sh \
            || die "NetBird install failed (see https://docs.netbird.io/get-started/install)"
    fi
    if [ -z "$(netbird_ip)" ]; then
        $ROOTDO netbird service install >/dev/null 2>&1 || true
        $ROOTDO netbird service start >/dev/null 2>&1 || true
        set -- up
        [ -n "$NB_SETUP_KEY" ] && set -- "$@" --setup-key "$NB_SETUP_KEY"
        [ -n "$NB_MGMT_URL" ]  && set -- "$@" --management-url "$NB_MGMT_URL"
        if [ -n "$NB_SETUP_KEY" ]; then
            say "Joining your NetBird network with the setup key"
        else
            say "Connecting this machine to NetBird — sign in with the URL below if asked"
        fi
        $ROOTDO netbird "$@" \
            || die "could not join NetBird — run 'netbird up' as root, then re-run this script"
        i=0
        while [ $i -lt 15 ] && [ -z "$(netbird_ip)" ]; do i=$((i + 1)); sleep 1; done
    fi
    [ -n "$(netbird_ip)" ] || die "no NetBird IP after setup (check: netbird status)"
}

lan_ip() {
    case "$OS" in
        Darwin) ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null ;;
        *)      ip route get 1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n1 ;;
    esac
}

# ── Bind addresses ──
# --tailscale / --netbird: only the VPN address(es), never the LAN or a public
# interface. Funnel and Tailscale Serve proxy to 127.0.0.1, so they add it.
# Otherwise a server listens on all interfaces, a personal machine on
# localhost only.
BINDS=""
add_bind() { case " $BINDS " in *" $1 "*) ;; *) BINDS="${BINDS:+$BINDS }$1" ;; esac; }
{ [ -n "$FUNNEL" ] || [ -n "$TS_SERVE" ]; } && ensure_tailscale
if [ -n "$TS_ONLY" ] || [ -n "$NB_ONLY" ]; then
    if [ -n "$TS_ONLY" ]; then ensure_tailscale; add_bind "$(tailscale_ip)"; fi
    if [ -n "$NB_ONLY" ]; then ensure_netbird;   add_bind "$(netbird_ip)"; fi
    { [ -n "$FUNNEL" ] || [ -n "$TS_SERVE" ]; } && add_bind "127.0.0.1"
elif [ "$MODE" = "server" ]; then
    add_bind "0.0.0.0"
else
    add_bind "127.0.0.1"
fi
HOST_ARGS=""
for b in $BINDS; do HOST_ARGS="${HOST_ARGS:+$HOST_ARGS }--host $b"; done

# Re-running this installer is safe: it never deletes your Garmin login or
# logs Tailscale/NetBird out. It only reinstalls the connector binary,
# (re)writes the service unit and (re)starts it.
GARMIN_MCP_CFG_DIR="${GARMIN_MCP_HOME:-$HOME/.config/garmin-mcp}"
[ -f "${GARMIN_MCP_CFG_DIR}/tokens/oauth2_token.json" ] \
    && say "Existing Garmin login found in ${GARMIN_MCP_CFG_DIR} — keeping it"
[ -n "$(tailscale_ip)" ] \
    && say "Existing Tailscale connection found — keeping it (not logging out)"
[ -n "$(netbird_ip)" ] \
    && say "Existing NetBird connection found — keeping it (not logging out)"
CHECK_HOST="${BINDS%% *}"
[ "$CHECK_HOST" = "0.0.0.0" ] && CHECK_HOST="127.0.0.1"
LINK="http://${CHECK_HOST}:${PORT}/setup"

# ── 1. Install the connector binary ──
target_triple() {
    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        armv7l|armv7*) arch="armv7" ;;
        *) return 1 ;;
    esac
    case "$OS" in
        Linux)
            if [ "$arch" = "armv7" ]; then echo "armv7-unknown-linux-musleabihf"
            else echo "${arch}-unknown-linux-musl"; fi ;;
        Darwin)
            [ "$arch" = "armv7" ] && return 1
            echo "${arch}-apple-darwin" ;;
        *) return 1 ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

install_prebuilt() {
    triple="$(target_triple)" || return 1
    tmp="$(mktemp -d)"
    asset="garmin-mcp-${triple}.tar.gz"
    if ! curl -fsSL -o "${tmp}/${asset}" "${RELEASE_URL}/${asset}" \
        || ! curl -fsSL -o "${tmp}/${asset}.sha256" "${RELEASE_URL}/${asset}.sha256"; then
        rm -rf "$tmp"; return 1
    fi
    want="$(cut -d' ' -f1 < "${tmp}/${asset}.sha256")"
    got="$(sha256_of "${tmp}/${asset}")"
    [ -n "$want" ] && [ "$want" = "$got" ] || { rm -rf "$tmp"; die "checksum mismatch for ${asset}"; }
    tar -xzf "${tmp}/${asset}" -C "$tmp" || { rm -rf "$tmp"; return 1; }
    mkdir -p "${APP_DIR}/bin"
    install -m 755 "${tmp}/garmin-mcp" "${EXE}.new" && mv -f "${EXE}.new" "$EXE"
    rm -rf "$tmp"
}

install_with_cargo() {
    command -v cargo >/dev/null 2>&1 || return 1
    if [ -n "$SRC" ]; then
        cargo install --quiet --locked --force --root "$APP_DIR" --path "$SRC"
    else
        cargo install --quiet --locked --force --root "$APP_DIR" --git "$REPO_GIT" garmin-mcp
    fi
}

say "Installing the Garmin MCP connector (user-only, ${APP_DIR})"
mkdir -p "$APP_DIR" "$BIN_DIR"
if [ -n "$SRC" ]; then
    install_with_cargo || die "cargo install failed ($SRC) — is Rust installed? (https://rustup.rs)"
elif install_prebuilt; then
    :
else
    echo "note: no prebuilt binary for $(uname -s)/$(uname -m) — building from source with cargo"
    install_with_cargo || {
        echo "Building from source needs Rust. Install it, then re-run this script:" >&2
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
        die "cargo not found"
    }
fi
[ -x "$EXE" ] || die "the connector binary is missing ($EXE)"
# Earlier (Python) versions lived in a virtualenv here; only code, no state.
rm -rf "${APP_DIR}/venv"
ln -sf "$EXE" "$BIN_DIR/garmin-mcp"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: add ${BIN_DIR} to your PATH to use the 'garmin-mcp' command." ;;
esac

# ── 2. cloudflared (optional — HTTPS link for claude.ai) ──
if ! command -v cloudflared >/dev/null 2>&1; then
    echo "note: for claude.ai in the browser, install cloudflared later:"
    case "$OS" in
        Darwin) echo "        brew install cloudflared" ;;
        *)      echo "        Arch: pacman -S cloudflared"
                echo "        Debian/Ubuntu: curl -fsSL -o /tmp/cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb && apt install -y /tmp/cloudflared.deb" ;;
    esac
fi

# ── 3. Start the connector (and keep it started across reboots) ──
STARTED=""
if [ "$OS" = "Linux" ] && [ -n "$IS_ROOT" ] && command -v systemctl >/dev/null 2>&1; then
    # Running as root (Proxmox LXC, VPS): system-wide unit, boots on its own,
    # no sudo and no lingering involved.
    cat > /etc/systemd/system/garmin-mcp.service <<EOF
[Unit]
Description=Garmin MCP local connector
After=network-online.target tailscaled.service netbird.service
Wants=network-online.target

[Service]
ExecStart=${EXE} serve ${HOST_ARGS} --port ${PORT}
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
ExecStart=${EXE} serve ${HOST_ARGS} --port ${PORT}
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
    HOST_XML=""
    for b in $BINDS; do HOST_XML="${HOST_XML}<string>--host</string><string>${b}</string>"; done
    cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
 "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.garmin-mcp.connector</string>
  <key>ProgramArguments</key>
  <array><string>${EXE}</string><string>serve</string>
         ${HOST_XML}
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
        # shellcheck disable=SC2086 # HOST_ARGS is a list of words
        nohup "$EXE" serve $HOST_ARGS --port "$PORT" \
            > "${APP_DIR}/connector.log" 2>&1 &
        STARTED="background"
    else
        STARTED="already-running"
    fi
fi

# ── 4. Boot persistence (user services need lingering; system units do not) ──
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

# ── 5. Wait until it answers, then hand over the links ──
i=0
while [ $i -lt 20 ]; do
    curl -fsS -m 1 "http://${CHECK_HOST}:${PORT}/health" >/dev/null 2>&1 && break
    i=$((i + 1)); sleep 0.5
done
curl -fsS -m 2 "http://${CHECK_HOST}:${PORT}/health" >/dev/null 2>&1 \
    || die "the connector did not start (try: ${EXE} serve ${HOST_ARGS})"

# ── 6. Stable HTTPS URL via Tailscale (--funnel: public Internet, or
#       --tailscale-serve: private to your tailnet only) ──
FUNNEL_URL=""
if [ -n "$FUNNEL" ] || [ -n "$TS_SERVE" ]; then
    if [ -n "$FUNNEL" ]; then
        say "Enabling the stable HTTPS address (Tailscale Funnel — public Internet)"
        TS_SUBCMD="funnel"
    else
        say "Enabling the stable HTTPS address (Tailscale Serve — private to your tailnet)"
        TS_SUBCMD="serve"
    fi
    if ! $ROOTDO tailscale "$TS_SUBCMD" --bg "$PORT"; then
        if [ -n "$FUNNEL" ]; then
            echo "If a link to enable Funnel for your tailnet was shown above, open it," >&2
            die "then re-run this script (tailscale funnel failed)"
        fi
        die "tailscale serve failed (check: tailscale status)"
    fi
    # "Self" comes before "Peer" in the status JSON: the first DNSName is ours.
    TSDNS="$($ROOTDO tailscale status --json 2>/dev/null | tr ',' '\n' \
        | sed -n 's/.*"DNSName": *"\([^"]*\)".*/\1/p' | head -n1)"
    TSDNS="${TSDNS%.}"
    TOKEN="$(cat "${GARMIN_MCP_CFG_DIR}/access_token" 2>/dev/null)"
    if [ -n "$TSDNS" ]; then
        FUNNEL_URL="https://${TSDNS}/garmin/?token=${TOKEN:-<run: garmin-mcp url>}"
    fi
    if [ -n "$FUNNEL" ]; then
        echo "" >&2
        echo "warning: Tailscale Funnel makes this URL reachable from the public" >&2
        echo "Internet, not just your tailnet — anyone who gets the token can call the" >&2
        echo "MCP endpoint (the token still gates every request; sign-in, health and" >&2
        echo "disconnect stay private). This is required for claude.ai's or Claude" >&2
        echo "Desktop's \"Add custom connector\" (it's called from Anthropic's own" >&2
        echo "servers, never your tailnet). If you're instead connecting a client that" >&2
        echo "runs directly on one of your own tailnet machines (e.g. Claude Code)," >&2
        echo "reinstall with:  sh get.sh --tailscale-serve" >&2
    fi
fi

say "Connector running (${STARTED}, listening on $(echo "$BINDS" | sed "s/\([^ ]*\)/\1:${PORT}/g"))."
echo ""
echo "  Sign in to Garmin here:"
for b in $BINDS; do
    if [ "$b" = "0.0.0.0" ]; then
        echo "    this machine   : http://127.0.0.1:${PORT}/setup"
        LAN="$(lan_ip)";       [ -n "$LAN" ] && echo "    from your LAN  : http://${LAN}:${PORT}/setup"
        TS="$(tailscale_ip)";  [ -n "$TS" ]  && echo "    via Tailscale  : http://${TS}:${PORT}/setup"
        NB="$(netbird_ip)";    [ -n "$NB" ]  && echo "    via NetBird    : http://${NB}:${PORT}/setup"
    elif [ "$b" = "127.0.0.1" ]; then
        echo "    this machine   : http://127.0.0.1:${PORT}/setup"
    elif [ "$b" = "$(tailscale_ip)" ]; then
        echo "    via Tailscale  : http://${b}:${PORT}/setup"
    elif [ "$b" = "$(netbird_ip)" ]; then
        echo "    via NetBird    : http://${b}:${PORT}/setup"
    else
        echo "    http://${b}:${PORT}/setup"
    fi
done
echo ""
if [ -n "$FUNNEL_URL" ]; then
    if [ -n "$FUNNEL" ]; then
        echo "  Stable HTTPS MCP URL for claude.ai (survives reboots, PUBLIC Internet):"
    else
        echo "  Stable HTTPS MCP URL (survives reboots, private to your tailnet):"
    fi
    echo "    ${FUNNEL_URL}"
    echo ""
fi
echo "Your email and password go from your browser to this machine to Garmin —"
echo "no third-party server ever sees them."
if [ "$BINDS" = "0.0.0.0" ]; then
    echo ""
    echo "warning: listening on all interfaces. Fine on a home LAN; on a VPS with a"
    echo "public IP, reinstall with:  sh get.sh --server --tailscale  (or --netbird)"
fi
if [ "$MODE" = "local" ]; then
    case "$OS" in
        Darwin) open "$LINK" >/dev/null 2>&1 || true ;;
        *)      command -v xdg-open >/dev/null 2>&1 && xdg-open "$LINK" >/dev/null 2>&1 || true ;;
    esac
fi
