//! HTTP server: setup page and API, minimal OAuth 2.1 server for claude.ai,
//! cloudflared tunnel control and the Garmin MCP endpoint.
//!
//! Credentials go browser → this process → Garmin, nowhere else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

use crate::config;
use crate::garmin::{Garmin, LoginOutcome, UnknownMfaSession};
use crate::net::is_wildcard;
use crate::tunnel::{Tunnel, TunnelError};

const SETUP_PAGE: &str = include_str!("setup_page.html");
const CODE_TTL: Duration = Duration::from_secs(600);

struct AuthCode {
    redirect_uri: String,
    code_challenge: String,
    created: Instant,
}

pub struct AppState {
    pub garmin: Garmin,
    pub tunnel: Tunnel,
    /// Where cloudflared should point: an address the server listens on.
    tunnel_target: String,
    oauth_codes: Mutex<HashMap<String, AuthCode>>,
}

type St = State<Arc<AppState>>;

pub fn state(binds: &[String], port: u16) -> Arc<AppState> {
    let host = binds
        .first()
        .filter(|h| !is_wildcard(h))
        .cloned()
        .unwrap_or_else(|| "127.0.0.1".into());
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    Arc::new(AppState {
        garmin: Garmin::new(),
        tunnel: Tunnel::default(),
        tunnel_target: format!("http://{host}:{port}"),
        oauth_codes: Mutex::new(HashMap::new()),
    })
}

/// The whole service: `guards` runs before routing (it may rewrite the path).
pub fn app(state: Arc<AppState>) -> Router {
    let origins: Vec<HeaderValue> = config::allowed_origins()
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request());

    let router = Router::new()
        .route("/", get(|| async { Redirect::temporary("/setup") }))
        .route("/setup", get(|| async { Html(SETUP_PAGE) }))
        .route("/health", get(health))
        .route("/api/setup/login", post(setup_login))
        .route("/api/setup/mfa", post(setup_mfa))
        .route("/api/disconnect", post(disconnect))
        .route("/api/tunnel/status", get(tunnel_status))
        .route("/api/tunnel/start", post(tunnel_start))
        .route("/api/tunnel/stop", post(tunnel_stop))
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/{*rest}",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route(
            "/.well-known/oauth-authorization-server/{*rest}",
            get(authorization_server),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize_page).post(authorize_submit))
        .route("/token", post(token))
        .route("/garmin", get(mcp_get).post(mcp_post).delete(mcp_get))
        .route("/garmin/", get(mcp_get).post(mcp_post).delete(mcp_get))
        .fallback(|| async { not_found() })
        .layer(cors)
        .with_state(state);

    // A layer on the outer router runs before the inner router matches, so
    // `guards` can rewrite /t/<token>/… paths.
    Router::new()
        .fallback_service(router)
        .layer(axum::middleware::from_fn(guards))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"detail": "Not Found"}))).into_response()
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

/// True when the request reached us through a public HTTPS entry point
/// (Cloudflare quick tunnel or Tailscale Funnel/Serve).
fn via_tunnel(h: &HeaderMap) -> bool {
    let host = header_str(h, "host");
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    h.contains_key("cf-ray") || host.ends_with(".trycloudflare.com") || host.ends_with(".ts.net")
}

fn public_base(h: &HeaderMap) -> String {
    let scheme = if via_tunnel(h) {
        "https"
    } else {
        match header_str(h, "x-forwarded-proto") {
            "" => "http",
            s => s,
        }
    };
    let host = match header_str(h, "host") {
        "" => format!("{}:{}", config::default_host(), config::default_port()),
        h => h.to_string(),
    };
    format!("{scheme}://{host}")
}

/// MCP URL as seen from the client that asked (localhost, LAN, Tailscale or
/// NetBird IP). An explicit GARMIN_MCP_PUBLIC_URL still wins.
fn client_mcp_url(h: &HeaderMap) -> Result<String> {
    if config::public_url().is_some() {
        return Ok(config::mcp_url(None)?);
    }
    let host = header_str(h, "host");
    if host.is_empty() {
        return Ok(config::mcp_url(None)?);
    }
    Ok(config::mcp_url(Some(&format!("http://{host}")))?)
}

fn token_error() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "invalid token"})),
    )
        .into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
        .into_response()
}

/// Strip the path-embedded token from logs: /t/<token>/x → /t/***/x.
fn loggable(path: &str) -> String {
    match path.strip_prefix("/t/") {
        Some(rest) => match rest.split_once('/') {
            Some((_, tail)) => format!("/t/***/{tail}"),
            None => "/t/***".into(),
        },
        None => path.to_string(),
    }
}

async fn guards(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let shown = loggable(req.uri().path());
    let resp = guarded(req, next).await;
    eprintln!("{method} {shown} → {}", resp.status().as_u16());
    resp
}

async fn guarded(mut req: Request, next: Next) -> Response {
    // Chrome Private Network Access: let an https page reach a private address.
    if req.method() == Method::OPTIONS {
        let mut resp = next.run(req).await;
        resp.headers_mut().insert(
            "access-control-allow-private-network",
            HeaderValue::from_static("true"),
        );
        return resp;
    }
    let Ok(token) = config::access_token() else {
        return internal("cannot read the access token");
    };

    // Path-embedded token (/t/<token>/garmin/…): some MCP clients drop query
    // parameters, so the token can live in the path instead.
    let mut path = req.uri().path().to_string();
    let mut path_token_ok = false;
    if let Some(rest) = path.strip_prefix("/t/") {
        match rest.split_once('/') {
            Some((t, tail)) if ct_eq(t, &token) => {
                path = format!("/{tail}");
                let pq = match req.uri().query() {
                    Some(q) => format!("{path}?{q}"),
                    None => path.clone(),
                };
                let mut parts = req.uri().clone().into_parts();
                parts.path_and_query = pq.parse().ok();
                if let Ok(uri) = Uri::from_parts(parts) {
                    *req.uri_mut() = uri;
                }
                path_token_ok = true;
            }
            _ => return token_error(),
        }
    }

    // Through a public entry point only the MCP endpoint and the OAuth surface
    // MCP clients need are reachable. Sign-in, health and disconnect stay
    // private; unknown paths answer 404 so probes learn nothing.
    if via_tunnel(req.headers()) {
        let public = path.starts_with("/garmin")
            || path.starts_with("/.well-known/")
            || path.starts_with("/authorize")
            || path == "/register"
            || path == "/token";
        if !public {
            return not_found();
        }
    }

    // The MCP endpoint requires the per-install access token.
    if path.starts_with("/garmin") && !path_token_ok {
        let mut supplied = req
            .uri()
            .query()
            .and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(k, _)| k == "token")
                    .map(|(_, v)| v.into_owned())
            })
            .unwrap_or_default();
        if supplied.is_empty() {
            let auth = header_str(req.headers(), "authorization");
            if auth.len() > 7 && auth[..7].eq_ignore_ascii_case("bearer ") {
                supplied = auth[7..].trim().to_string();
            }
        }
        if !ct_eq(&supplied, &token) {
            let mut resp = token_error();
            let meta = format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/garmin/\"",
                public_base(req.headers())
            );
            if let Ok(v) = HeaderValue::from_str(&meta) {
                resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
            }
            return resp;
        }
    }

    next.run(req).await
}

// ── MCP endpoint ───────────────────────────────────────────

async fn mcp_post(State(st): St, body: Bytes) -> Response {
    crate::mcp::handle(&st.garmin, body).await
}

/// Stateless server: no server-initiated SSE stream, no sessions to delete.
async fn mcp_get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

// ── Setup page API ─────────────────────────────────────────

async fn health(headers: HeaderMap) -> Response {
    let connected = Garmin::is_connected();
    let mut payload = json!({"status": "ok", "connected": connected});
    if connected {
        match client_mcp_url(&headers) {
            Ok(u) => payload["mcp_url"] = json!(u),
            Err(e) => return internal(e),
        }
    }
    Json(payload).into_response()
}

fn parse_json(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

async fn setup_login(State(st): St, headers: HeaderMap, body: Bytes) -> Response {
    let body = parse_json(&body);
    let email = body["email"].as_str().unwrap_or_default().trim();
    let password = body["password"].as_str().unwrap_or_default();
    if email.is_empty() || password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Email and password are required"})),
        )
            .into_response();
    }
    match st.garmin.login(email, password).await {
        Ok(LoginOutcome::NeedsMfa(id)) => {
            Json(json!({"mfa_required": true, "session_id": id})).into_response()
        }
        Ok(LoginOutcome::Connected) => match client_mcp_url(&headers) {
            Ok(u) => Json(json!({"mcp_url": u})).into_response(),
            Err(e) => internal(e),
        },
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": format!("Garmin login failed: {e:#}")})),
        )
            .into_response(),
    }
}

async fn setup_mfa(State(st): St, headers: HeaderMap, body: Bytes) -> Response {
    let body = parse_json(&body);
    let id = body["session_id"].as_str().unwrap_or_default();
    let code = body["mfa_code"].as_str().unwrap_or_default().trim();
    match st.garmin.resume_mfa(id, code).await {
        Ok(()) => match client_mcp_url(&headers) {
            Ok(u) => Json(json!({"mcp_url": u})).into_response(),
            Err(e) => internal(e),
        },
        Err(e) if e.is::<UnknownMfaSession>() => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": format!("MFA verification failed: {e:#}")})),
        )
            .into_response(),
    }
}

async fn disconnect(State(st): St) -> Response {
    match st.garmin.disconnect().await {
        Ok(()) => Json(json!({"status": "disconnected"})).into_response(),
        Err(e) => internal(e),
    }
}

// ── HTTPS tunnel (cloudflared), managed from the setup page ──

async fn tunnel_status(State(st): St) -> Response {
    let url = st.tunnel.url().await;
    let mcp = match &url {
        Some(base) => match config::mcp_url(Some(base)) {
            Ok(u) => Some(u),
            Err(e) => return internal(e),
        },
        None => None,
    };
    Json(json!({
        "cloudflared": crate::net::which("cloudflared").is_some(),
        "running": url.is_some(),
        "url": mcp,
    }))
    .into_response()
}

async fn tunnel_start(State(st): St) -> Response {
    match st.tunnel.start(&st.tunnel_target).await {
        Ok(base) => match config::mcp_url(Some(&base)) {
            Ok(u) => Json(json!({"url": u})).into_response(),
            Err(e) => internal(e),
        },
        Err(TunnelError::Missing) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "cloudflared_missing"})),
        )
            .into_response(),
        Err(TunnelError::Timeout) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"error": "Tunnel did not come up within 30s"})),
        )
            .into_response(),
        Err(TunnelError::Spawn(e)) => internal(e),
    }
}

async fn tunnel_stop(State(st): St) -> Response {
    st.tunnel.stop().await;
    Json(json!({"status": "stopped"})).into_response()
}

// ── Minimal OAuth 2.1 server (what claude.ai requires for custom connectors) ──
//
// The "authorization" is simply proving you own this install: the consent page
// asks for the connector's access token, and the issued OAuth access token IS
// that same token, which the /garmin guard already accepts as Bearer.

async fn protected_resource(headers: HeaderMap) -> Json<Value> {
    let base = public_base(&headers);
    Json(json!({
        "resource": format!("{base}/garmin/"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
    }))
}

async fn authorization_server(headers: HeaderMap) -> Json<Value> {
    let base = public_base(&headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["garmin"],
    }))
}

async fn register(body: Bytes) -> Response {
    let body = parse_json(&body);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let redirect_uris = body
        .get("redirect_uris")
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or(json!([]));
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": config::random_token(16),
            "client_id_issued_at": now,
            "redirect_uris": redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "client_name": body["client_name"].as_str().unwrap_or_default(),
        })),
    )
        .into_response()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn form(body: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

fn field<'a>(f: &'a [(String, String)], k: &str) -> &'a str {
    f.iter()
        .find(|(x, _)| x == k)
        .map(|(_, v)| v.as_str())
        .unwrap_or_default()
}

fn consent_page(params: &[(String, String)], error: &str) -> Response {
    let fields: String = params
        .iter()
        .map(|(k, v)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}" />"#,
                esc(k),
                esc(v)
            )
        })
        .collect();
    let err = if error.is_empty() {
        String::new()
    } else {
        format!(r#"<p class="err">{}</p>"#, esc(error))
    };
    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1.0"/>
<title>Authorize — Garmin MCP</title><style>
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
background:#0a0e13;color:#e6edf3;min-height:100vh;display:grid;place-items:center;padding:1.5rem}}
.card{{max-width:400px;width:100%;background:#10161d;border:1px solid #1e2833;
border-radius:16px;padding:2rem}}
h1{{font-size:1.15rem;margin:0 0 .5rem}}
p{{font-size:.86rem;color:#8b98a9;line-height:1.55;margin:0 0 1rem}}
.err{{color:#f87171}}
input[type=password]{{width:100%;box-sizing:border-box;background:#0b1016;
border:1px solid #223041;border-radius:10px;padding:.7rem .85rem;font-size:.92rem;
color:#e6edf3;outline:none;margin-bottom:.9rem}}
button{{width:100%;background:#2563eb;color:#fff;border:none;border-radius:10px;
padding:.75rem;font-size:.95rem;font-weight:600;cursor:pointer}}
code{{font-size:.85em;color:#9fc1ee}}
</style></head><body><div class="card">
<h1>Authorize access to Garmin MCP</h1>
<p>A Claude client is asking to use this connector. Paste your connector
access token to approve (find it with <code>garmin-mcp url</code> on the
server, or on the setup page).</p>
{err}
<form method="post" action="/authorize">
{fields}
<input type="password" name="access_token" placeholder="Connector access token" required autofocus />
<button type="submit">Authorize</button>
</form></div></body></html>"#
    ))
    .into_response()
}

async fn authorize_page(uri: Uri) -> Response {
    let params = form(uri.query().unwrap_or_default().as_bytes());
    if field(&params, "redirect_uri").is_empty() || field(&params, "client_id").is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_request"})),
        )
            .into_response();
    }
    consent_page(&params, "")
}

async fn authorize_submit(State(st): St, body: Bytes) -> Response {
    let mut f = form(&body);
    let supplied = field(&f, "access_token").trim().to_string();
    f.retain(|(k, _)| k != "access_token");
    let Ok(token) = config::access_token() else {
        return internal("cannot read the access token");
    };
    if !ct_eq(&supplied, &token) {
        return consent_page(&f, "Invalid access token — try again.");
    }
    let redirect_uri = field(&f, "redirect_uri").to_string();
    let Ok(mut target) = url::Url::parse(&redirect_uri) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_request"})),
        )
            .into_response();
    };
    let code = config::random_token(24);
    {
        let mut codes = st.oauth_codes.lock().unwrap();
        codes.retain(|_, c| c.created.elapsed() < CODE_TTL);
        codes.insert(
            code.clone(),
            AuthCode {
                redirect_uri,
                code_challenge: field(&f, "code_challenge").to_string(),
                created: Instant::now(),
            },
        );
    }
    {
        let mut q = target.query_pairs_mut();
        q.append_pair("code", &code);
        let state = field(&f, "state");
        if !state.is_empty() {
            q.append_pair("state", state);
        }
    }
    (StatusCode::FOUND, [(header::LOCATION, target.to_string())]).into_response()
}

pub(crate) fn pkce_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn token(State(st): St, body: Bytes) -> Response {
    let f = form(&body);
    let grant_error = || {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response()
    };
    let Ok(access) = config::access_token() else {
        return internal("cannot read the access token");
    };
    match field(&f, "grant_type") {
        "refresh_token" => {
            if !ct_eq(field(&f, "refresh_token"), &access) {
                return grant_error();
            }
        }
        "authorization_code" => {
            let data = st.oauth_codes.lock().unwrap().remove(field(&f, "code"));
            let Some(data) = data.filter(|d| d.created.elapsed() < CODE_TTL) else {
                return grant_error();
            };
            if data.redirect_uri != field(&f, "redirect_uri") {
                return grant_error();
            }
            if !data.code_challenge.is_empty()
                && !ct_eq(&pkce_s256(field(&f, "code_verifier")), &data.code_challenge)
            {
                return grant_error();
            }
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "unsupported_grant_type"})),
            )
                .into_response();
        }
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": 31536000,
            "refresh_token": access,
            "scope": "garmin",
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_python() {
        // base64.urlsafe_b64encode(sha256(v)).rstrip("=") in Python
        assert_eq!(
            pkce_s256("dBjjuFE5lhqTSBePg4yCd2uORUiMHhHvT_ztGaGxlv8"),
            "iGguaUwr_VOVKHqmTuJ0t_QLyeq93MaqkaNKN3QqK9U"
        );
    }

    #[test]
    fn hides_path_token_in_logs() {
        assert_eq!(loggable("/t/secret/garmin/"), "/t/***/garmin/");
        assert_eq!(loggable("/garmin/"), "/garmin/");
    }

    #[test]
    fn detects_public_entry_points() {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_static("box.tail1234.ts.net"));
        assert!(via_tunnel(&h));
        h.insert("host", HeaderValue::from_static("100.92.4.17:8765"));
        assert!(!via_tunnel(&h));
        h.insert("host", HeaderValue::from_static("box.netbird.cloud:8765"));
        assert!(!via_tunnel(&h));
    }
}
