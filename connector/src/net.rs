//! Addresses this machine can be reached on: LAN, Tailscale, NetBird.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

/// The LAN address the default route goes out of (no packet is sent).
pub fn lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

async fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let exe = which(cmd)?;
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(exe).args(args).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Tailscale IPv4, only while Tailscale is up (`tailscale ip -4` still
/// answers when it is stopped).
pub async fn tailscale_ip() -> Option<String> {
    let out = run("tailscale", &["status", "--json"]).await?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    if v["BackendState"] != "Running" {
        return None;
    }
    v["TailscaleIPs"]
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str())
        .find(|ip| ip.contains('.'))
        .map(String::from)
}

/// The NetBird overlay IPv4 of this peer, or the `wt0` interface address on
/// Linux when the daemon socket isn't readable by this user.
pub async fn netbird_ip() -> Option<String> {
    if let Some(out) = run("netbird", &["status", "--ipv4"]).await {
        let ip = out.trim().split('/').next().unwrap_or_default();
        if ip.parse::<std::net::Ipv4Addr>().is_ok() {
            return Some(ip.to_string());
        }
    }
    let out = run("ip", &["-4", "-o", "addr", "show", "dev", "wt0"]).await?;
    out.split_whitespace()
        .skip_while(|w| *w != "inet")
        .nth(1)
        .and_then(|cidr| cidr.split('/').next())
        .map(String::from)
}

/// The NetBird DNS name of this peer (e.g. "server.netbird.cloud"). The
/// top-level `fqdn`: each entry of `peers` has its own.
pub async fn netbird_fqdn() -> Option<String> {
    let out = run("netbird", &["status", "--json"]).await?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    v["fqdn"]
        .as_str()
        .map(|s| s.trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty())
}

pub fn is_wildcard(host: &str) -> bool {
    matches!(host, "0.0.0.0" | "::" | "[::]")
}

/// Hosts a browser can use to reach a server bound to `binds`.
pub async fn reachable_hosts(binds: &[String]) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let push = |h: String, label: &'static str, out: &mut Vec<(String, &'static str)>| {
        if !out.iter().any(|(x, _)| *x == h) {
            out.push((h, label));
        }
    };
    for b in binds {
        if is_wildcard(b) {
            push("127.0.0.1".into(), "this machine", &mut out);
            if let Some(ip) = lan_ip() {
                push(ip, "your LAN", &mut out);
            }
            if let Some(ip) = tailscale_ip().await {
                push(ip, "Tailscale", &mut out);
            }
            if let Some(ip) = netbird_ip().await {
                push(ip, "NetBird", &mut out);
                if let Some(name) = netbird_fqdn().await {
                    push(name, "NetBird DNS", &mut out);
                }
            }
        } else {
            push(b.clone(), "", &mut out);
        }
    }
    out
}
