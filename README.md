# Garmin MCP

> 🇫🇷 [Version française](README.fr.md)

Link your Garmin account to Claude — with **zero third-party server**. One command
installs a small connector on your own machine (or home server); your Garmin
email and password go from your browser to that machine to Garmin, and nowhere
else. Nobody — no hosting provider, not this project — can ever see them.

Works with **Claude Code**, **Claude Desktop**, **claude.ai** (browser) and any
MCP client. Tools: activities, activity details, daily summary, sleep, heart
rate, Body Battery, stress, HRV, user profile.

## Install

**Personal machine** (Linux & macOS):

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh
```

**Home server / Proxmox LXC / VPS** — pick one:

```sh
# LAN + Tailscale access (home server):
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --server

# Bind to the Tailscale IP only (required on a VPS with a public IP):
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --server --tailscale

# Everything above + a permanent public HTTPS URL for claude.ai (Tailscale Funnel):
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --funnel
```

The script installs everything under the current user, installs Tailscale if
needed (`--tailscale`/`--funnel`), registers the connector as a service that
starts with the machine (systemd system unit as root, systemd user unit +
lingering otherwise, launchd on macOS) and prints the sign-in links.
`--no-sudo` forbids any privilege escalation (implicit when running as root).

## Sign in to Garmin

Open the setup page printed by the installer — `http://127.0.0.1:8765/setup`
locally, or `http://<server-ip>:8765/setup` from your LAN/tailnet — and sign in
with your Garmin account (MFA supported). Sign-in endpoints only answer on
private addresses, never on the public HTTPS entry points.

## Connect a Claude client

| Client | What to use |
|---|---|
| **Claude Code** | `claude mcp add -s user --transport http garmin "<local or tailscale MCP URL>"` |
| **Claude Desktop** / local MCP clients | The MCP URL shown by the setup page (`http://…:8765/garmin/?token=…`) |
| **claude.ai** (browser) | The public HTTPS URL **without token** — see below |

### claude.ai (OAuth)

claude.ai requires HTTPS and an OAuth flow for custom connectors. The connector
implements both:

1. Get a public HTTPS address: `--funnel` at install time (permanent
   `https://<machine>.<tailnet>.ts.net`, survives reboots) or the
   **Create HTTPS link** button on the setup page (Cloudflare quick tunnel,
   URL changes at each restart).
2. In claude.ai → Settings → Connectors → Add custom connector, paste the
   HTTPS base URL followed by `/garmin/` (no token), e.g.:
   `https://garmin.tailXXXX.ts.net/garmin/`
3. claude.ai opens the connector's authorization page: paste your access token
   (shown by `garmin-mcp url` or the setup page) and approve. Done — the
   authorization persists.

If claude.ai reports it cannot reach the server right after enabling Funnel,
the `.ts.net` DNS record may still be propagating; wait a few minutes. If it
persists, rename the machine in the Tailscale admin console (a fresh DNS name
resolves immediately) and re-run the installer.

## CLI

```sh
garmin-mcp               # start the connector (default: serve)
garmin-mcp login         # sign in from the terminal instead of the page
garmin-mcp url           # print your MCP URL (--base <url> for a public base)
garmin-mcp tunnel        # Cloudflare quick tunnel from the terminal
garmin-mcp logout        # disconnect Garmin (MCP token unchanged)
garmin-mcp rotate-token  # new access token — every pasted URL/authorization dies
```

## How it works

```
Browser ──▶ garmin-mcp connector (your machine) ──▶ Garmin SSO + Connect API
                    │  OAuth tokens in ~/.config/garmin-mcp/ (never the password)
                    ├──▶ MCP endpoint /garmin/  ◀── Claude Code / Desktop (token URL)
                    └──▶ public HTTPS entry     ◀── claude.ai (OAuth 2.1 + PKCE)
```

- The MCP endpoint accepts the per-install token as `?token=`, as a path prefix
  (`/t/<token>/garmin/`), or as a `Bearer` header — the OAuth flow issues that
  same token.
- Through public entry points (Funnel, quick tunnel), only the MCP endpoint and
  the OAuth surface are reachable; sign-in, health and disconnect answer on
  private addresses only, and unknown paths return 404.
- Internet scanners probing a public address are normal; everything they touch
  returns 404.

### Proxmox LXC

The script runs as root without ever calling `sudo`, and installs a system-wide
systemd unit that starts with the CT. Tailscale needs `/dev/net/tun`: if the
container doesn't have it, the script stops and prints the exact two lines to
add to `/etc/pve/lxc/<CTID>.conf` on the Proxmox host. Debian containers also
need `apt install python3-venv` first.

## License

MIT
