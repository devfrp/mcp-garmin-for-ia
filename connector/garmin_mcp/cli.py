import argparse
import getpass
import re
import shutil
import socket
import subprocess
import sys

from . import __version__, garmin
from .config import HOST, PORT, mcp_url


def _lan_ip() -> str | None:
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("1.1.1.1", 80))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except OSError:
        return None


def _tailscale_ip() -> str | None:
    exe = shutil.which("tailscale")
    if not exe:
        return None
    try:
        out = subprocess.run([exe, "ip", "-4"], capture_output=True,
                             text=True, timeout=5)
        ip = out.stdout.strip().splitlines()
        return ip[0] if out.returncode == 0 and ip else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def _reachable_hosts(bind_host: str) -> list[str]:
    if bind_host != "0.0.0.0":
        return [bind_host]
    hosts = ["127.0.0.1"]
    for ip in (_lan_ip(), _tailscale_ip()):
        if ip and ip not in hosts:
            hosts.append(ip)
    return hosts


def cmd_serve(args):
    from .server import serve

    print(f"Garmin MCP connector listening on http://{args.host}:{args.port}")
    for h in _reachable_hosts(args.host):
        print(f"Setup page: http://{h}:{args.port}/setup")
    if garmin.is_connected():
        print(f"MCP URL: {mcp_url()}")
    else:
        print("No Garmin account connected yet — open the setup page "
              "or run `garmin-mcp login`.")
    serve(host=args.host, port=args.port)


def cmd_login(args):
    email = input("Garmin account email: ").strip()
    password = getpass.getpass("Password: ")
    try:
        status, session_id = garmin.login(email, password)
        if status == "needs_mfa":
            code = input("MFA code: ").strip()
            garmin.resume_mfa(session_id, code)
    except Exception as exc:
        print(f"Login failed: {exc}", file=sys.stderr)
        sys.exit(1)
    print("Connected!")
    print(f"MCP URL: {mcp_url()}")
    print("Paste it into Claude Desktop (Settings > Connectors > Add custom connector).")


def cmd_url(args):
    if not garmin.is_connected():
        print("No Garmin account connected. Run `garmin-mcp login` first.",
              file=sys.stderr)
        sys.exit(1)
    print(mcp_url(base=args.base))


def _probe(host: str, port: int) -> bool:
    import urllib.request
    try:
        urllib.request.urlopen(f"http://{host}:{port}/health", timeout=2)
        return True
    except OSError:
        return False


def cmd_tunnel(args):
    """Expose only the MCP endpoint over HTTPS (needed by claude.ai in the browser)."""
    # Find where the connector actually listens (127.0.0.1, or the Tailscale
    # IP on --tailscale installs).
    target = None
    for cand in ("127.0.0.1", _tailscale_ip(), _lan_ip()):
        if cand and _probe(cand, args.port):
            target = cand
            break
    if target is None:
        print("The connector is not running (no /health answer). "
              "Start it first: garmin-mcp serve", file=sys.stderr)
        sys.exit(1)
    exe = shutil.which("cloudflared")
    if not exe:
        print("cloudflared is required for the HTTPS tunnel. Install it:", file=sys.stderr)
        print("  Arch          : pacman -S cloudflared", file=sys.stderr)
        print("  Debian/Ubuntu : curl -fsSL -o /tmp/cloudflared.deb "
              "https://github.com/cloudflare/cloudflared/releases/latest/download/"
              "cloudflared-linux-amd64.deb && apt install -y /tmp/cloudflared.deb",
              file=sys.stderr)
        print("  macOS         : brew install cloudflared", file=sys.stderr)
        sys.exit(1)
    if not garmin.is_connected():
        print("note: no Garmin account connected yet — the URL will work "
              "once you sign in.")

    proc = subprocess.Popen(
        [exe, "tunnel", "--url", f"http://{target}:{args.port}"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    print("Starting HTTPS tunnel (Cloudflare quick tunnel)…")
    try:
        for line in proc.stdout:
            m = re.search(r"https://[a-z0-9-]+\.trycloudflare\.com", line)
            if m:
                print()
                print(f"MCP URL (HTTPS): {mcp_url(base=m.group(0))}")
                print("Paste it into claude.ai > Settings > Connectors "
                      "> Add custom connector.")
                print("note: this URL changes on every tunnel restart. "
                      "Keep this command running while you use Claude. Ctrl-C to stop.")
                print()
        proc.wait()
    except KeyboardInterrupt:
        proc.terminate()
    sys.exit(proc.returncode or 0)


def cmd_logout(args):
    garmin.disconnect()
    print("Disconnected. Garmin tokens deleted (MCP URL unchanged).")


def cmd_rotate_token(args):
    from .config import ACCESS_TOKEN_FILE

    ACCESS_TOKEN_FILE.unlink(missing_ok=True)
    print("Access token rotated. Update the URL everywhere it was pasted:")
    print(mcp_url())


def main():
    parser = argparse.ArgumentParser(
        prog="garmin-mcp",
        description="Local Garmin MCP connector — credentials never leave this machine.",
    )
    parser.add_argument("--version", action="version", version=__version__)
    sub = parser.add_subparsers(dest="command")

    p_serve = sub.add_parser("serve", help="start the local connector (default)")
    p_serve.add_argument("--host", default=HOST)
    p_serve.add_argument("--port", type=int, default=PORT)
    p_serve.set_defaults(func=cmd_serve)

    p_login = sub.add_parser("login", help="sign in to Garmin from the terminal")
    p_login.set_defaults(func=cmd_login)

    p_url = sub.add_parser("url", help="print your MCP connector URL")
    p_url.add_argument("--base", help="public base URL (e.g. an HTTPS tunnel)")
    p_url.set_defaults(func=cmd_url)

    p_tunnel = sub.add_parser(
        "tunnel", help="expose the MCP endpoint over HTTPS for claude.ai"
    )
    p_tunnel.add_argument("--port", type=int, default=PORT)
    p_tunnel.set_defaults(func=cmd_tunnel)

    p_logout = sub.add_parser("logout", help="disconnect and delete Garmin tokens")
    p_logout.set_defaults(func=cmd_logout)

    p_rotate = sub.add_parser(
        "rotate-token", help="generate a new MCP access token (old URLs stop working)"
    )
    p_rotate.set_defaults(func=cmd_rotate_token)

    args = parser.parse_args()
    if not args.command:
        args.host, args.port = HOST, PORT
        cmd_serve(args)
    else:
        args.func(args)


if __name__ == "__main__":
    main()
