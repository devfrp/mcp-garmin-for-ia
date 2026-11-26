//! Cloudflare quick tunnel (`cloudflared tunnel --url …`): a temporary public
//! HTTPS address, needed by claude.ai's "Add custom connector".

use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

use crate::net::which;

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https://[a-z0-9-]+\.trycloudflare\.com").unwrap());

#[derive(Debug)]
pub enum TunnelError {
    Missing,
    Timeout,
    Spawn(std::io::Error),
}

pub struct Spawned {
    pub child: Child,
    pub url: watch::Receiver<Option<String>>,
}

/// Start cloudflared towards `target` (e.g. "http://127.0.0.1:8765"). Its
/// output is drained for as long as it runs, and the public URL is published
/// on `url` as soon as cloudflared prints it.
pub fn spawn_cloudflared(target: &str, echo: bool) -> Result<Spawned, TunnelError> {
    let exe = which("cloudflared").ok_or(TunnelError::Missing)?;
    let mut child = Command::new(exe)
        .args(["tunnel", "--no-autoupdate", "--url", target])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(TunnelError::Spawn)?;
    let (tx, rx) = watch::channel(None);
    let tx = Arc::new(tx);
    fn drain<R: AsyncRead + Unpin + Send + 'static>(
        r: R,
        tx: Arc<watch::Sender<Option<String>>>,
        echo: bool,
    ) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if echo {
                    eprintln!("cloudflared: {line}");
                }
                if let Some(m) = URL_RE.find(&line) {
                    tx.send_if_modified(|v| {
                        v.is_none()
                            .then(|| *v = Some(m.as_str().to_string()))
                            .is_some()
                    });
                }
            }
        });
    }
    if let Some(out) = child.stdout.take() {
        drain(out, tx.clone(), echo);
    }
    if let Some(err) = child.stderr.take() {
        drain(err, tx, echo);
    }
    Ok(Spawned { child, url: rx })
}

/// Wait until the tunnel prints its URL (or gives up / exits).
pub async fn wait_url(rx: &mut watch::Receiver<Option<String>>) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(30), rx.wait_for(Option::is_some))
        .await
        .ok()?
        .ok()
        .and_then(|v| v.clone())
}

/// The tunnel managed from the setup page.
#[derive(Default)]
pub struct Tunnel {
    inner: Mutex<Option<Spawned>>,
}

impl Tunnel {
    pub async fn start(&self, target: &str) -> Result<String, TunnelError> {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.as_mut()
            && matches!(s.child.try_wait(), Ok(None))
            && let Some(url) = s.url.borrow().clone()
        {
            return Ok(url);
        }
        let mut s = spawn_cloudflared(target, false)?;
        match wait_url(&mut s.url).await {
            Some(url) => {
                *g = Some(s);
                Ok(url)
            }
            None => {
                let _ = s.child.start_kill();
                *g = None;
                Err(TunnelError::Timeout)
            }
        }
    }

    /// The public base URL while the tunnel is up.
    pub async fn url(&self) -> Option<String> {
        let mut g = self.inner.lock().await;
        let s = g.as_mut()?;
        if !matches!(s.child.try_wait(), Ok(None)) {
            *g = None;
            return None;
        }
        s.url.borrow().clone()
    }

    pub async fn stop(&self) {
        if let Some(mut s) = self.inner.lock().await.take() {
            let _ = s.child.start_kill();
            let _ = s.child.wait().await;
        }
    }
}
