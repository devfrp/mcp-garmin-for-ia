# garmin-mcp

Local Garmin Connect MCP connector, in Rust: one static binary, no runtime.
See the [project README](../README.md) ([français](../README.fr.md)).

```sh
cargo install --locked --git https://github.com/devfrp/mcp-garmin-for-ia.git garmin-mcp
garmin-mcp
```

| Module | Role |
|---|---|
| `src/garmin.rs` | Garmin SSO sign-in (MFA), OAuth1 signing, OAuth2 exchange/refresh, Connect API |
| `src/mcp.rs` | MCP Streamable HTTP (stateless JSON), the nine tools |
| `src/server.rs` | HTTP routes, access guard, OAuth 2.1 + PKCE for claude.ai, setup API |
| `src/net.rs` | LAN / Tailscale / NetBird address detection |
| `src/tunnel.rs` | Cloudflare quick tunnel |
| `src/config.rs` | Paths, environment, access token |

Tokens live in `~/.config/garmin-mcp/` (`GARMIN_MCP_HOME`), same format as
the former Python version (garth), so upgrades keep the login.
