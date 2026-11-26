//! Paths, environment settings and the per-install MCP access token.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub const DEFAULT_PORT: u16 = 8765;

pub fn config_dir() -> PathBuf {
    if let Some(p) = env::var_os("GARMIN_MCP_HOME") {
        return PathBuf::from(p);
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("garmin-mcp")
}

pub fn tokens_dir() -> PathBuf {
    config_dir().join("tokens")
}

fn access_token_file() -> PathBuf {
    config_dir().join("access_token")
}

pub fn default_host() -> String {
    env::var("GARMIN_MCP_HOST").unwrap_or_else(|_| "127.0.0.1".into())
}

pub fn default_port() -> u16 {
    env::var("GARMIN_MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub fn public_url() -> Option<String> {
    env::var("GARMIN_MCP_PUBLIC_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// Origins allowed to call the setup API from a browser. Extend with
/// GARMIN_MCP_EXTRA_ORIGINS (comma-separated).
pub fn allowed_origins() -> Vec<String> {
    let mut origins: Vec<String> = [
        "https://claude.ai",
        "https://www.claude.ai",
        "http://localhost:8765",
        "http://127.0.0.1:8765",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Ok(extra) = env::var("GARMIN_MCP_EXTRA_ORIGINS") {
        origins.extend(
            extra
                .split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(String::from),
        );
    }
    origins
}

/// `n` random bytes, URL-safe base64 without padding (like Python's
/// `secrets.token_urlsafe(n)`).
pub fn random_token(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("OS random generator unavailable");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Write a file readable by the current user only, atomically.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)
}

pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// The persistent MCP access token, created on first use.
pub fn access_token() -> io::Result<String> {
    let file = access_token_file();
    if let Ok(t) = fs::read_to_string(&file) {
        let t = t.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    let token = random_token(24);
    write_private(&file, token.as_bytes())?;
    Ok(token)
}

/// Replace the access token: every URL or authorization handed out dies.
pub fn rotate_access_token() -> io::Result<String> {
    let token = random_token(24);
    write_private(&access_token_file(), token.as_bytes())?;
    Ok(token)
}

/// The MCP URL with its token. `base` wins, then GARMIN_MCP_PUBLIC_URL, then
/// the default bind address.
pub fn mcp_url(base: Option<&str>) -> io::Result<String> {
    let base = match base.map(|b| b.trim_end_matches('/').to_string()) {
        Some(b) if !b.is_empty() => b,
        _ => {
            public_url().unwrap_or_else(|| format!("http://{}:{}", default_host(), default_port()))
        }
    };
    Ok(format!("{base}/garmin/?token={}", access_token()?))
}
