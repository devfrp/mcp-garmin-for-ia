//! Garmin Connect access: SSO sign-in (with MFA), OAuth1 → OAuth2 exchange,
//! token refresh and Connect API calls. Same flow and on-disk token format as
//! the `garth` Python library, so existing logins keep working. Everything
//! stays on this machine; the password is never stored.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Sha1;
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use crate::config;

const DOMAIN: &str = "garmin.com";
const OAUTH_CONSUMER_URL: &str = "https://thegarth.s3.amazonaws.com/oauth_consumer.json";
// Garmin's Cloudflare rejects the mobile User-Agent on SSO pages since the
// 2025 auth-flow change; a desktop browser UA passes (login, MFA and resume).
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
// The OAuth endpoints, on the other hand, expect the mobile app.
const MOBILE_UA: &str = "com.garmin.android.apps.connectmobile";
const MFA_TTL: Duration = Duration::from_secs(15 * 60);

static CSRF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"name="_csrf"\s+value="(.+?)""#).unwrap());
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<title>(.+?)</title>").unwrap());
static TICKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"embed\?ticket=([^"]+)""#).unwrap());

/// Raised by tools when no Garmin account is connected.
#[derive(Debug)]
pub struct NotConnected;

impl std::fmt::Display for NotConnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("No Garmin account connected. Run `garmin-mcp login` or use the setup page.")
    }
}

impl std::error::Error for NotConnected {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth1Token {
    pub oauth_token: String,
    pub oauth_token_secret: String,
    #[serde(default)]
    pub mfa_token: Option<String>,
    #[serde(default)]
    pub mfa_expiration_timestamp: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2Token {
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub jti: String,
    pub token_type: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub refresh_token_expires_in: i64,
    #[serde(default)]
    pub refresh_token_expires_at: i64,
}

impl OAuth2Token {
    fn expired(&self) -> bool {
        // A minute of margin so a token never dies mid-request.
        self.expires_at < now_secs() + 60
    }

    fn authorization(&self) -> String {
        let mut t = self.token_type.to_lowercase();
        if let Some(first) = t.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        format!("{t} {}", self.access_token)
    }
}

#[derive(Deserialize)]
struct Consumer {
    consumer_key: String,
    consumer_secret: String,
}

pub enum LoginOutcome {
    Connected,
    NeedsMfa(String),
}

#[derive(Default)]
struct Session {
    oauth1: Option<OAuth1Token>,
    oauth2: Option<OAuth2Token>,
    /// mtime of oauth2_token.json when loaded: a CLI login/logout or a
    /// refresh by another process is picked up without a restart.
    loaded_mtime: Option<SystemTime>,
    display_name: Option<String>,
}

/// A browser-like SSO conversation (cookies + last page), kept between the
/// password step and the MFA step.
struct Sso {
    http: reqwest::Client,
    last_url: Option<String>,
    last_text: String,
}

struct PendingMfa {
    sso: Sso,
    created: Instant,
}

pub struct Garmin {
    session: Mutex<Session>,
    mfa: StdMutex<HashMap<String, PendingMfa>>,
    consumer: OnceCell<Consumer>,
    http: reqwest::Client,
}

impl Default for Garmin {
    fn default() -> Self {
        Self::new()
    }
}

impl Garmin {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(Session::default()),
            mfa: StdMutex::new(HashMap::new()),
            consumer: OnceCell::new(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP client"),
        }
    }

    pub fn is_connected() -> bool {
        config::tokens_dir().join("oauth2_token.json").exists()
    }

    // ── Sign-in ─────────────────────────────────────────────

    /// Start a Garmin login. Returns `Connected`, or `NeedsMfa(session_id)`
    /// to be completed with [`Garmin::resume_mfa`].
    pub async fn login(&self, email: &str, password: &str) -> Result<LoginOutcome> {
        let mut sso = Sso::new()?;
        let sso_base = format!("https://sso.{DOMAIN}/sso");
        let embed = format!("{sso_base}/embed");

        // Set cookies
        sso.get(
            &embed,
            &[
                ("id", "gauth-widget"),
                ("embedWidget", "true"),
                ("gauthHost", &sso_base),
            ],
            false,
        )
        .await?;

        // Get the CSRF token
        let signin = format!("{sso_base}/signin");
        sso.get(&signin, &signin_params(), true).await?;
        let csrf = find(&CSRF_RE, &sso.last_text, "CSRF token")?;

        // Submit email and password
        sso.post(
            &signin,
            &signin_params(),
            &[
                ("username", email),
                ("password", password),
                ("embed", "true"),
                ("_csrf", &csrf),
            ],
        )
        .await?;
        let title = find(&TITLE_RE, &sso.last_text, "page title")?;

        if title.contains("MFA") {
            let id = config::random_token(16);
            let mut pending = self.mfa.lock().unwrap();
            pending.retain(|_, p| p.created.elapsed() < MFA_TTL);
            pending.insert(
                id.clone(),
                PendingMfa {
                    sso,
                    created: Instant::now(),
                },
            );
            return Ok(LoginOutcome::NeedsMfa(id));
        }
        if title != "Success" {
            bail!("unexpected Garmin page: {title} (wrong email or password?)");
        }
        self.complete_login(&sso).await?;
        Ok(LoginOutcome::Connected)
    }

    pub async fn resume_mfa(&self, session_id: &str, code: &str) -> Result<()> {
        let pending = self
            .mfa
            .lock()
            .unwrap()
            .remove(session_id)
            .filter(|p| p.created.elapsed() < MFA_TTL);
        let Some(PendingMfa { mut sso, .. }) = pending else {
            return Err(anyhow!(UnknownMfaSession));
        };
        let csrf = find(&CSRF_RE, &sso.last_text, "CSRF token")?;
        let url = format!("https://sso.{DOMAIN}/sso/verifyMFA/loginEnterMfaCode");
        sso.post(
            &url,
            &signin_params(),
            &[
                ("mfa-code", code),
                ("embed", "true"),
                ("_csrf", &csrf),
                ("fromPage", "setupEnterMfaCode"),
            ],
        )
        .await?;
        if !TICKET_RE.is_match(&sso.last_text) {
            let title = TITLE_RE
                .captures(&sso.last_text)
                .map(|c| c[1].to_string())
                .unwrap_or_default();
            bail!("Garmin did not accept the code ({title})");
        }
        self.complete_login(&sso).await
    }

    async fn complete_login(&self, sso: &Sso) -> Result<()> {
        let ticket = find(&TICKET_RE, &sso.last_text, "login ticket")?;
        let oauth1 = self.preauthorize(&ticket).await?;
        let oauth2 = self.exchange(&oauth1).await?;
        let dir = config::tokens_dir();
        write_json(&dir.join("oauth1_token.json"), &oauth1)?;
        write_json(&dir.join("oauth2_token.json"), &oauth2)?;
        let mut s = self.session.lock().await;
        *s = Session {
            oauth1: Some(oauth1),
            oauth2: Some(oauth2),
            loaded_mtime: mtime(&dir.join("oauth2_token.json")),
            display_name: None,
        };
        Ok(())
    }

    /// Forget the Garmin account. The MCP access token is kept on purpose so
    /// connector URLs pasted into Claude survive re-logins; rotate it
    /// explicitly with `garmin-mcp rotate-token`.
    pub async fn disconnect(&self) -> Result<()> {
        *self.session.lock().await = Session::default();
        match fs::remove_dir_all(config::tokens_dir()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    // ── OAuth1 / OAuth2 ─────────────────────────────────────

    async fn consumer(&self) -> Result<&Consumer> {
        self.consumer
            .get_or_try_init(|| async {
                let resp = self
                    .http
                    .get(OAUTH_CONSUMER_URL)
                    .send()
                    .await?
                    .error_for_status()?;
                Ok::<_, anyhow::Error>(serde_json::from_slice(&resp.bytes().await?)?)
            })
            .await
            .context("could not fetch the Garmin OAuth consumer")
    }

    async fn preauthorize(&self, ticket: &str) -> Result<OAuth1Token> {
        let c = self.consumer().await?;
        let mut url = Url::parse(&format!(
            "https://connectapi.{DOMAIN}/oauth-service/oauth/preauthorized"
        ))?;
        url.query_pairs_mut()
            .append_pair("ticket", ticket)
            .append_pair("login-url", &format!("https://sso.{DOMAIN}/sso/embed"))
            .append_pair("accepts-mfa-tokens", "true");
        let auth = oauth1_header("GET", &url, &[], c, None);
        let resp = self
            .http
            .get(url)
            .header("User-Agent", MOBILE_UA)
            .header("Authorization", auth)
            .send()
            .await?;
        let body = checked(resp, "OAuth1 preauthorization").await?;
        let map: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        let get = |k: &str| map.get(k).cloned();
        Ok(OAuth1Token {
            oauth_token: get("oauth_token").context("no oauth_token from Garmin")?,
            oauth_token_secret: get("oauth_token_secret")
                .context("no oauth_token_secret from Garmin")?,
            mfa_token: get("mfa_token"),
            mfa_expiration_timestamp: get("mfa_expiration_timestamp"),
            domain: Some(DOMAIN.into()),
        })
    }

    async fn exchange(&self, oauth1: &OAuth1Token) -> Result<OAuth2Token> {
        let c = self.consumer().await?;
        let domain = oauth1.domain.as_deref().unwrap_or(DOMAIN);
        let url = Url::parse(&format!(
            "https://connectapi.{domain}/oauth-service/oauth/exchange/user/2.0"
        ))?;
        let body: Vec<(String, String)> = oauth1
            .mfa_token
            .iter()
            .map(|t| ("mfa_token".to_string(), t.clone()))
            .collect();
        let auth = oauth1_header(
            "POST",
            &url,
            &body,
            c,
            Some((&oauth1.oauth_token, &oauth1.oauth_token_secret)),
        );
        let resp = self
            .http
            .post(url)
            .header("User-Agent", MOBILE_UA)
            .header("Authorization", auth)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_encode(
                body.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            ))
            .send()
            .await?;
        let body = checked(resp, "OAuth2 exchange").await?;
        let mut t: OAuth2Token =
            serde_json::from_str(&body).context("unexpected OAuth2 answer from Garmin")?;
        let now = now_secs();
        t.expires_at = now + t.expires_in;
        t.refresh_token_expires_at = now + t.refresh_token_expires_in;
        Ok(t)
    }

    /// Current `Authorization` header value, (re)loading tokens from disk and
    /// refreshing the OAuth2 token when needed.
    async fn authorization(&self) -> Result<String> {
        let path = config::tokens_dir().join("oauth2_token.json");
        let mut s = self.session.lock().await;
        let on_disk = mtime(&path);
        if on_disk.is_none() {
            *s = Session::default();
            return Err(NotConnected.into());
        }
        if s.oauth2.is_none() || s.loaded_mtime != on_disk {
            let dir = config::tokens_dir();
            *s = Session {
                oauth1: Some(read_json(&dir.join("oauth1_token.json"))?),
                oauth2: Some(read_json(&path)?),
                loaded_mtime: on_disk,
                display_name: None,
            };
        }
        if s.oauth2.as_ref().is_some_and(OAuth2Token::expired) {
            // Garmin itself re-exchanges the OAuth1 token rather than using
            // the OAuth2 refresh token.
            let oauth1 = s.oauth1.clone().ok_or(NotConnected)?;
            let fresh = self
                .exchange(&oauth1)
                .await
                .context("refreshing Garmin token")?;
            write_json(&path, &fresh)?;
            s.oauth2 = Some(fresh);
            s.loaded_mtime = mtime(&path);
        }
        Ok(s.oauth2.as_ref().ok_or(NotConnected)?.authorization())
    }

    // ── Connect API ─────────────────────────────────────────

    pub async fn api(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let auth = self.authorization().await?;
        let mut url = Url::parse(&format!("https://connectapi.{DOMAIN}{path}"))?;
        if !params.is_empty() {
            let mut q = url.query_pairs_mut();
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        let mut delay = Duration::from_millis(500);
        for attempt in 0.. {
            let sent = self
                .http
                .get(url.clone())
                .header("User-Agent", BROWSER_UA)
                .header("Authorization", &auth)
                .send()
                .await;
            let retryable = match &sent {
                Ok(r) => matches!(r.status().as_u16(), 408 | 500 | 502 | 503 | 504),
                Err(e) => e.is_timeout() || e.is_connect(),
            };
            if retryable && attempt < 3 {
                tokio::time::sleep(delay).await;
                delay *= 2;
                continue;
            }
            let resp = sent?;
            if resp.status().as_u16() == 204 {
                return Ok(Value::Null);
            }
            let body = checked(resp, path).await?;
            if body.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&body).context("Garmin returned invalid JSON");
        }
        unreachable!()
    }

    pub async fn display_name(&self) -> Result<String> {
        if let Some(n) = self.session.lock().await.display_name.clone() {
            return Ok(n);
        }
        let profile = self.api("/userprofile-service/socialProfile", &[]).await?;
        let name = profile["displayName"]
            .as_str()
            .context("no displayName in the Garmin profile")?
            .to_string();
        self.session.lock().await.display_name = Some(name.clone());
        Ok(name)
    }
}

#[derive(Debug)]
pub struct UnknownMfaSession;

impl std::fmt::Display for UnknownMfaSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unknown or expired MFA session. Sign in again.")
    }
}

impl std::error::Error for UnknownMfaSession {}

impl Sso {
    fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .cookie_store(true)
                .user_agent(BROWSER_UA)
                .timeout(Duration::from_secs(30))
                .build()?,
            last_url: None,
            last_text: String::new(),
        })
    }

    async fn get(&mut self, url: &str, params: &[(&str, &str)], referer: bool) -> Result<()> {
        let mut url = Url::parse(url)?;
        url.query_pairs_mut().extend_pairs(params);
        let mut req = self.http.get(url);
        if referer && let Some(r) = &self.last_url {
            req = req.header("Referer", r);
        }
        self.record(req.send().await?).await
    }

    async fn post(
        &mut self,
        url: &str,
        params: &[(&str, &str)],
        form: &[(&str, &str)],
    ) -> Result<()> {
        let mut url = Url::parse(url)?;
        url.query_pairs_mut().extend_pairs(params);
        let mut req = self
            .http
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_encode(form.iter().copied()));
        if let Some(r) = &self.last_url {
            req = req.header("Referer", r);
        }
        self.record(req.send().await?).await
    }

    async fn record(&mut self, resp: reqwest::Response) -> Result<()> {
        self.last_url = Some(resp.url().to_string());
        self.last_text = checked(resp, "Garmin sign-in").await?;
        Ok(())
    }
}

fn signin_params() -> Vec<(&'static str, &'static str)> {
    const EMBED: &str = "https://sso.garmin.com/sso/embed";
    vec![
        ("id", "gauth-widget"),
        ("embedWidget", "true"),
        ("gauthHost", EMBED),
        ("service", EMBED),
        ("source", EMBED),
        ("redirectAfterAccountLoginUrl", EMBED),
        ("redirectAfterAccountCreationUrl", EMBED),
    ]
}

async fn checked(resp: reqwest::Response, what: &str) -> Result<String> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => " (refused — wrong credentials, or Garmin is blocking this network)",
            429 => " (too many attempts — wait a while before retrying)",
            _ => "",
        };
        bail!("{what}: Garmin answered HTTP {}{hint}", status.as_u16());
    }
    Ok(text)
}

fn find(re: &Regex, text: &str, what: &str) -> Result<String> {
    re.captures(text)
        .map(|c| c[1].to_string())
        .ok_or_else(|| anyhow!("couldn't find the {what} in Garmin's answer"))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&raw).with_context(|| format!("parsing {}", path.display()))
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    config::write_private(path, &serde_json::to_vec_pretty(v)?)
        .with_context(|| format!("writing {}", path.display()))
}

fn form_encode<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

// ── OAuth 1.0a request signing (HMAC-SHA1, RFC 5849) ───────

const RFC3986: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn pct(s: &str) -> String {
    utf8_percent_encode(s, RFC3986).to_string()
}

pub(crate) fn oauth1_signature(
    method: &str,
    base_url: &str,
    params: &[(String, String)],
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    let mut enc: Vec<(String, String)> = params.iter().map(|(k, v)| (pct(k), pct(v))).collect();
    enc.sort();
    let joined = enc
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let base = format!(
        "{}&{}&{}",
        method.to_uppercase(),
        pct(base_url),
        pct(&joined)
    );
    let key = format!("{}&{}", pct(consumer_secret), pct(token_secret));
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key");
    mac.update(base.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

fn oauth1_header(
    method: &str,
    url: &Url,
    body: &[(String, String)],
    c: &Consumer,
    token: Option<(&str, &str)>,
) -> String {
    let mut oauth: Vec<(String, String)> = vec![
        ("oauth_consumer_key".into(), c.consumer_key.clone()),
        (
            "oauth_nonce".into(),
            config::random_token(24).replace(['-', '_'], ""),
        ),
        ("oauth_signature_method".into(), "HMAC-SHA1".into()),
        ("oauth_timestamp".into(), now_secs().to_string()),
        ("oauth_version".into(), "1.0".into()),
    ];
    if let Some((t, _)) = token {
        oauth.push(("oauth_token".into(), t.into()));
    }
    let mut all = oauth.clone();
    all.extend(url.query_pairs().into_owned());
    all.extend(body.iter().cloned());
    let base_url = format!(
        "{}://{}{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port().map(|p| format!(":{p}")).unwrap_or_default(),
        url.path()
    );
    let sig = oauth1_signature(
        method,
        &base_url,
        &all,
        &c.consumer_secret,
        token.map(|(_, s)| s).unwrap_or(""),
    );
    oauth.push(("oauth_signature".into(), sig));
    let fields = oauth
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", pct(v)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("OAuth {fields}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from Twitter's "Creating a signature" guide.
    #[test]
    fn oauth1_signature_matches_reference() {
        let p = |k: &str, v: &str| (k.to_string(), v.to_string());
        let params = vec![
            p(
                "status",
                "Hello Ladies + Gentlemen, a signed OAuth request!",
            ),
            p("include_entities", "true"),
            p("oauth_consumer_key", "xvz1evFS4wEEPTGEFPHBog"),
            p("oauth_nonce", "kYjzVBB8Y0ZFabxSWbWovY3uYSQ2pTgmZeNu2VS4cg"),
            p("oauth_signature_method", "HMAC-SHA1"),
            p("oauth_timestamp", "1318622958"),
            p(
                "oauth_token",
                "370773112-GmHxMAgYyLbNEtIKZeRNFsMKPR9EyMZeS9weJAEb",
            ),
            p("oauth_version", "1.0"),
        ];
        let sig = oauth1_signature(
            "POST",
            "https://api.twitter.com/1.1/statuses/update.json",
            &params,
            "kAcSOqF21Fu85e7zjz7ZN2U4ZRhfV3WpwPAoE3Z7kBw",
            "LswwdoUaIvS8ltyTt5jkRh4J50vUPVVHtR2YPi5kE",
        );
        assert_eq!(sig, "hCtSmYh+iHYCEqBWrE7C7hYmtUk=");
    }

    #[test]
    fn parses_sso_pages() {
        let page = r#"<title>Success</title><input type="hidden" name="_csrf" value="ABC123" />
            <a href="https://sso.garmin.com/sso/embed?ticket=ST-0123-abc-cas">"#;
        assert_eq!(find(&TITLE_RE, page, "t").unwrap(), "Success");
        assert_eq!(find(&CSRF_RE, page, "c").unwrap(), "ABC123");
        assert_eq!(find(&TICKET_RE, page, "k").unwrap(), "ST-0123-abc-cas");
    }

    #[test]
    fn reads_garth_token_files() {
        let o1: OAuth1Token = serde_json::from_str(
            r#"{"oauth_token":"a","oauth_token_secret":"b","mfa_token":null,
                "mfa_expiration_timestamp":"2026-01-01T00:00:00","domain":"garmin.com"}"#,
        )
        .unwrap();
        assert_eq!(o1.oauth_token, "a");
        let o2: OAuth2Token = serde_json::from_str(
            r#"{"scope":"s","jti":"j","token_type":"bearer","access_token":"x",
                "refresh_token":"r","expires_in":3600,"expires_at":1,
                "refresh_token_expires_in":7200,"refresh_token_expires_at":2}"#,
        )
        .unwrap();
        assert!(o2.expired());
        assert_eq!(o2.authorization(), "Bearer x");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Talks to the real Garmin SSO (no credentials sent):
    /// `cargo test -- --ignored live`
    #[tokio::test]
    #[ignore]
    async fn sso_signin_page_and_consumer() {
        let mut sso = Sso::new().unwrap();
        let base = format!("https://sso.{DOMAIN}/sso");
        sso.get(
            &format!("{base}/embed"),
            &[
                ("id", "gauth-widget"),
                ("embedWidget", "true"),
                ("gauthHost", &base),
            ],
            false,
        )
        .await
        .unwrap();
        sso.get(&format!("{base}/signin"), &signin_params(), true)
            .await
            .unwrap();
        assert!(find(&CSRF_RE, &sso.last_text, "CSRF token").is_ok());
        let g = Garmin::new();
        assert!(!g.consumer().await.unwrap().consumer_key.is_empty());
    }
}
