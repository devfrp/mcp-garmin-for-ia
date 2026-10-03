# Garmin MCP

*Read this in: **English** · [Français](README.fr.md)*

🌐 **Website:** <https://devfrp.github.io/mcp-garmin-for-ia/>

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

# Permanent HTTPS URL, PRIVATE to your tailnet — for Claude Code or any
# client you run yourself on a machine in your tailnet. NOT usable for
# claude.ai's "Add custom connector" (see warning below — that needs --funnel):
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --tailscale-serve

# Permanent HTTPS URL, PUBLIC — required for claude.ai / Claude Desktop's
# "Add custom connector". See the warning below before using this.
curl -fsSL https://raw.githubusercontent.com/devfrp/mcp-garmin-for-ia/main/get.sh | sh -s -- --funnel
```

The script installs everything under the current user, installs Tailscale if
needed (`--tailscale`/`--tailscale-serve`/`--funnel`), registers the connector
as a service that starts with the machine (systemd system unit as root,
systemd user unit + lingering otherwise, launchd on macOS) and prints the
sign-in links. `--no-sudo` forbids any privilege escalation (implicit when
running as root). **Safe to re-run** any time (to switch flags or update):
it never deletes your Garmin login or logs Tailscale out, it only reinstalls
the connector code and restarts the service.

> [!WARNING]
> `--funnel` makes your connector's MCP endpoint and OAuth surface reachable
> from the **public Internet**, not just your tailnet — anyone who obtains
> the access token can call it (sign-in, health and disconnect stay private,
> and unknown paths 404, but the token is the only gate on the MCP endpoint
> itself). Use `--tailscale-serve` instead if you only need the URL for a
> client that runs directly on one of your own tailnet machines (Claude Code,
> a local script, etc).
>
> **`--tailscale-serve` does *not* work for claude.ai's or Claude Desktop's
> "Add custom connector"**, even from a device on your tailnet: that feature
> is verified and called from Anthropic's own servers, which are never on
> your tailnet. For that specific flow you need a public address — `--funnel`
> or the Cloudflare quick tunnel are the only options.

## Sign in to Garmin

Open the setup page printed by the installer — `http://127.0.0.1:8765/setup`
locally, or `http://<server-ip>:8765/setup` from your LAN/tailnet — and sign in
with your Garmin account (MFA supported). Sign-in endpoints only answer on
these private addresses, never on the stable HTTPS address (Funnel or
Tailscale Serve) or a Cloudflare quick tunnel.

## Connect a Claude client

There are two different ways to connect a client, and they don't accept the
same kind of URL:

| Client | What to use |
|---|---|
| **Claude Code** — `claude mcp add -s user --transport http garmin "<URL>"` | Any URL reachable **directly from the machine running Claude Code**: the LAN/Tailscale token URL, or even a `--tailscale-serve` HTTPS address if that machine is on your tailnet. |
| **Claude Desktop** — local MCP server entry in `claude_desktop_config.json` | Same as above — the token URL shown by the setup page (`http://…:8765/garmin/?token=…`), reachable directly from that machine. |
| **claude.ai** (browser) **or** Claude Desktop's **"Add custom connector"** dialog | A **public** HTTPS URL only — `--funnel` or the Cloudflare quick tunnel. This feature is verified and called from Anthropic's own servers, so it can never reach a `--tailscale-serve` address, no matter what machine or network you're on. |

### claude.ai / "Add custom connector" (OAuth)

This flow requires HTTPS and OAuth, and — unlike the local clients above — the
connection to your server is made by Anthropic's infrastructure, not your
device. It needs a **public** address:

1. Get a public HTTPS address — pick one:
   - `--funnel` at install time: permanent
     `https://<machine>.<tailnet>.ts.net`, survives reboots. ⚠️ See the
     warning above — this exposes the MCP endpoint to the whole Internet.
   - The **Create HTTPS link** button on the setup page: a Cloudflare quick
     tunnel, also public, and its URL changes at each restart.
2. In claude.ai → Settings → Connectors → Add custom connector (or Claude
   Desktop's equivalent dialog), paste the HTTPS base URL followed by
   `/garmin/` (no token), e.g.: `https://garmin.tailXXXX.ts.net/garmin/`
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
                    ├──▶ MCP endpoint /garmin/  ◀── local clients, direct (token URL or
                    │                                --tailscale-serve): Claude Code,
                    │                                Desktop's local config, any client
                    │                                running on a machine in your tailnet
                    └──▶ public HTTPS entry (Funnel  ◀── claude.ai / "Add custom connector"
                         or quick tunnel)                 — called from Anthropic's servers
                                                           (OAuth 2.1 + PKCE)
```

- The MCP endpoint accepts the per-install token as `?token=`, as a path prefix
  (`/t/<token>/garmin/`), or as a `Bearer` header — the OAuth flow issues that
  same token.
- Through the public HTTPS entry point (Funnel, quick tunnel), only the MCP
  endpoint and the OAuth surface are reachable; sign-in, health and disconnect
  answer on private addresses only, and unknown paths return 404.
- Funnel and the Cloudflare quick tunnel make that entry reachable from the
  **public Internet** — internet scanners probing it are normal; everything
  they touch returns 404, but the access token is what actually protects the
  MCP endpoint itself, so treat it like a password.
- `--tailscale-serve` gives a separate, private HTTPS address for direct local
  clients (Claude Code, etc.) running on your tailnet — it is a different
  address from the public entry above and is never reachable by claude.ai's
  "Add custom connector", which always needs the public one.

### Proxmox LXC

The script runs as root without ever calling `sudo`, and installs a system-wide
systemd unit that starts with the CT. Tailscale needs `/dev/net/tun`: if the
container doesn't have it, the script stops and prints the exact two lines to
add to `/etc/pve/lxc/<CTID>.conf` on the Proxmox host. Debian containers also
need `apt install python3-venv` first.

## License

MIT
