//! garmin-mcp — local Garmin Connect MCP connector. Credentials never leave
//! this machine.

mod config;
mod garmin;
mod mcp;
mod net;
mod server;
mod tunnel;

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::garmin::{Garmin, LoginOutcome};

#[derive(Parser)]
#[command(
    name = "garmin-mcp",
    version,
    about = "Local Garmin MCP connector — credentials never leave this machine."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the local connector (default)
    Serve {
        /// Address to listen on; repeat (or comma-separate) to listen on
        /// several, e.g. `--host 127.0.0.1 --host 100.92.4.17`
        #[arg(long, value_delimiter = ',')]
        host: Vec<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Sign in to Garmin from the terminal
    Login,
    /// Print your MCP connector URL
    Url {
        /// Public base URL (e.g. an HTTPS tunnel)
        #[arg(long)]
        base: Option<String>,
    },
    /// Expose the MCP endpoint over HTTPS for claude.ai (Cloudflare quick tunnel)
    Tunnel {
        #[arg(long)]
        port: Option<u16>,
    },
    /// Disconnect and delete Garmin tokens
    Logout,
    /// Generate a new MCP access token (old URLs stop working)
    RotateToken,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Cmd::Serve {
        host: vec![],
        port: None,
    }) {
        Cmd::Serve { host, port } => serve(host, port).await,
        Cmd::Login => login().await,
        Cmd::Url { base } => url(base),
        Cmd::Tunnel { port } => tunnel(port).await,
        Cmd::Logout => logout().await,
        Cmd::RotateToken => rotate_token(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(hosts: Vec<String>, port: Option<u16>) -> anyhow::Result<ExitCode> {
    let mut hosts: Vec<String> = hosts
        .into_iter()
        .map(|h| h.trim().trim_matches(['[', ']']).to_string())
        .filter(|h| !h.is_empty())
        .collect();
    if hosts.is_empty() {
        hosts = config::default_host()
            .split(',')
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect();
    }
    let port = port.unwrap_or_else(config::default_port);
    config::access_token()?; // create it now, not on the first request

    let mut listeners = Vec::new();
    for h in &hosts {
        let l = tokio::net::TcpListener::bind((h.as_str(), port))
            .await
            .map_err(|e| anyhow::anyhow!("cannot listen on {h}:{port}: {e}"))?;
        println!(
            "Garmin MCP connector listening on http://{}",
            l.local_addr()?
        );
        listeners.push(l);
    }
    for (h, label) in net::reachable_hosts(&hosts).await {
        let h = if h.contains(':') { format!("[{h}]") } else { h };
        if label.is_empty() {
            println!("Setup page: http://{h}:{port}/setup");
        } else {
            println!("Setup page ({label}): http://{h}:{port}/setup");
        }
    }
    if Garmin::is_connected() {
        println!("MCP URL: {}", config::mcp_url(None)?);
    } else {
        println!(
            "No Garmin account connected yet — open the setup page or run `garmin-mcp login`."
        );
    }

    let state = server::state(&hosts, port);
    let app = server::app(state.clone());
    let mut tasks = tokio::task::JoinSet::new();
    for l in listeners {
        let app = app.clone();
        tasks.spawn(async move { axum::serve(l, app).await });
    }
    tokio::select! {
        r = tasks.join_next() => {
            if let Some(Ok(Err(e))) = r {
                eprintln!("server error: {e}");
            }
        }
        _ = shutdown_signal() => {}
    }
    state.tunnel.stop().await;
    Ok(ExitCode::SUCCESS)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn prompt(label: &str) -> io::Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().lock().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

async fn login() -> anyhow::Result<ExitCode> {
    let email = prompt("Garmin account email: ")?;
    let password = rpassword::prompt_password("Password: ")?;
    let g = Garmin::new();
    let outcome = match g.login(&email, &password).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Login failed: {e:#}");
            return Ok(ExitCode::FAILURE);
        }
    };
    if let LoginOutcome::NeedsMfa(id) = outcome {
        let code = prompt("MFA code: ")?;
        if let Err(e) = g.resume_mfa(&id, &code).await {
            eprintln!("Login failed: {e:#}");
            return Ok(ExitCode::FAILURE);
        }
    }
    println!("Connected!");
    println!("MCP URL: {}", config::mcp_url(None)?);
    println!("Paste it into Claude Code / Claude Desktop, or see the setup page for claude.ai.");
    Ok(ExitCode::SUCCESS)
}

fn url(base: Option<String>) -> anyhow::Result<ExitCode> {
    if !Garmin::is_connected() {
        eprintln!("No Garmin account connected. Run `garmin-mcp login` first.");
        return Ok(ExitCode::FAILURE);
    }
    println!("{}", config::mcp_url(base.as_deref())?);
    Ok(ExitCode::SUCCESS)
}

async fn probe(host: &str, port: u16) -> bool {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client
        .get(format!("http://{host}:{port}/health"))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

async fn tunnel(port: Option<u16>) -> anyhow::Result<ExitCode> {
    let port = port.unwrap_or_else(config::default_port);
    // Find where the connector actually listens (127.0.0.1, or a VPN IP on
    // --tailscale / --netbird installs).
    let mut candidates = vec![Some("127.0.0.1".to_string())];
    candidates.push(net::tailscale_ip().await);
    candidates.push(net::netbird_ip().await);
    candidates.push(net::lan_ip());
    let mut target = None;
    for c in candidates.into_iter().flatten() {
        if probe(&c, port).await {
            target = Some(c);
            break;
        }
    }
    let Some(target) = target else {
        eprintln!(
            "The connector is not running (no /health answer). Start it first: garmin-mcp serve"
        );
        return Ok(ExitCode::FAILURE);
    };
    if net::which("cloudflared").is_none() {
        eprintln!("cloudflared is required for the HTTPS tunnel. Install it:");
        eprintln!("  Arch          : pacman -S cloudflared");
        eprintln!(
            "  Debian/Ubuntu : curl -fsSL -o /tmp/cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb && apt install -y /tmp/cloudflared.deb"
        );
        eprintln!("  macOS         : brew install cloudflared");
        return Ok(ExitCode::FAILURE);
    }
    if !Garmin::is_connected() {
        println!("note: no Garmin account connected yet — the URL will work once you sign in.");
    }
    println!("Starting HTTPS tunnel (Cloudflare quick tunnel)…");
    let mut t = match tunnel::spawn_cloudflared(&format!("http://{target}:{port}"), false) {
        Ok(t) => t,
        Err(e) => anyhow::bail!("could not start cloudflared: {e:?}"),
    };
    match tunnel::wait_url(&mut t.url).await {
        Some(base) => {
            println!();
            println!("MCP URL (HTTPS): {}", config::mcp_url(Some(&base))?);
            println!("Paste it into claude.ai > Settings > Connectors > Add custom connector.");
            println!(
                "note: this URL changes on every tunnel restart. Keep this command running while you use Claude. Ctrl-C to stop."
            );
            println!();
        }
        None => eprintln!("cloudflared did not print a tunnel URL within 30s."),
    }
    tokio::select! {
        s = t.child.wait() => Ok(if s?.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE }),
        _ = shutdown_signal() => {
            let _ = t.child.start_kill();
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn logout() -> anyhow::Result<ExitCode> {
    Garmin::new().disconnect().await?;
    println!("Disconnected. Garmin tokens deleted (MCP URL unchanged).");
    Ok(ExitCode::SUCCESS)
}

fn rotate_token() -> anyhow::Result<ExitCode> {
    config::rotate_access_token()?;
    println!("Access token rotated. Update the URL everywhere it was pasted:");
    println!("{}", config::mcp_url(None)?);
    Ok(ExitCode::SUCCESS)
}
