"""Local HTTP server: setup API for the web page + Garmin MCP endpoint.

Binds to 127.0.0.1 only. Credentials go browser -> this process -> Garmin.
"""

import base64
import hashlib
import hmac
import html as _html
import os
import re
import secrets
import shutil
import subprocess
import threading
import time
from contextlib import asynccontextmanager
from importlib import resources
from urllib.parse import urlencode

import uvicorn
from fastapi import FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import HTMLResponse, JSONResponse, RedirectResponse
from mcp.server.fastmcp import FastMCP
from mcp.server.transport_security import TransportSecuritySettings

from . import garmin
from .config import ALLOWED_ORIGINS, HOST, PORT, get_access_token, mcp_url

# Host validation (DNS-rebinding protection) is relaxed because the endpoint is
# reached through HTTPS tunnels with dynamic hostnames; the access token checked
# in the middleware below is what actually gates every MCP request.
mcp = FastMCP(
    "Garmin",
    stateless_http=True,
    transport_security=TransportSecuritySettings(
        enable_dns_rebinding_protection=False
    ),
)
mcp.settings.streamable_http_path = "/"


# ── MCP tools ─────────────────────────────────────────────


@mcp.tool()
def get_user_profile() -> dict:
    """Get the Garmin user profile (name, locale, activity level)."""
    return garmin.api("/userprofile-service/socialProfile")


@mcp.tool()
def get_activities(limit: int = 10, start: int = 0) -> list:
    """List recent activities (runs, rides, swims...). Paginate with start/limit."""
    return garmin.api(
        "/activitylist-service/activities/search/activities",
        limit=limit,
        start=start,
    )


@mcp.tool()
def get_activity_details(activity_id: int) -> dict:
    """Get full details for one activity by its ID."""
    return garmin.api(f"/activity-service/activity/{activity_id}")


@mcp.tool()
def get_daily_summary(date: str) -> dict:
    """Daily wellness summary (steps, calories, distance...) for a date (YYYY-MM-DD)."""
    return garmin.api(
        f"/usersummary-service/usersummary/daily/{garmin.display_name()}",
        calendarDate=date,
    )


@mcp.tool()
def get_sleep(date: str) -> dict:
    """Sleep data (stages, score, duration) for a date (YYYY-MM-DD)."""
    return garmin.api(
        f"/wellness-service/wellness/dailySleepData/{garmin.display_name()}",
        date=date,
        nonSleepBufferMinutes=60,
    )


@mcp.tool()
def get_heart_rate(date: str) -> dict:
    """Daily heart rate values for a date (YYYY-MM-DD)."""
    return garmin.api(
        f"/wellness-service/wellness/dailyHeartRate/{garmin.display_name()}",
        date=date,
    )


@mcp.tool()
def get_body_battery(date: str) -> list:
    """Body Battery report for a date (YYYY-MM-DD)."""
    return garmin.api(
        "/wellness-service/wellness/bodyBattery/reports/daily",
        startDate=date,
        endDate=date,
    )


@mcp.tool()
def get_stress(date: str) -> dict:
    """Daily stress levels for a date (YYYY-MM-DD)."""
    return garmin.api(f"/wellness-service/wellness/dailyStress/{date}")


@mcp.tool()
def get_hrv(date: str) -> dict:
    """Heart rate variability (HRV) data for a date (YYYY-MM-DD)."""
    return garmin.api(f"/hrv-service/hrv/{date}")


# ── FastAPI app ───────────────────────────────────────────


@asynccontextmanager
async def lifespan(app: FastAPI):
    async with mcp.session_manager.run():
        yield


app = FastAPI(lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=ALLOWED_ORIGINS,
    allow_methods=["*"],
    allow_headers=["*"],
)


def _via_tunnel(request: Request) -> bool:
    """True when the request reached us through a public HTTPS entry point
    (Cloudflare quick tunnel or Tailscale Funnel)."""
    host = request.headers.get("host", "").split(":")[0]
    return (
        "cf-ray" in request.headers
        or host.endswith(".trycloudflare.com")
        or host.endswith(".ts.net")
    )


@app.middleware("http")
async def guards(request: Request, call_next):
    # Chrome Private Network Access: allow the https page to reach 127.0.0.1
    if request.method == "OPTIONS":
        response = await call_next(request)
        response.headers["Access-Control-Allow-Private-Network"] = "true"
        return response

    # Path-embedded token (/t/<token>/garmin/...): some MCP clients drop query
    # parameters, so the token can live in the path instead. Strip the prefix
    # and remember that this request is already authenticated. All later
    # checks must use the rewritten `path`, not request.url (cached).
    path = request.url.path
    path_token_ok = False
    if path.startswith("/t/"):
        parts = path.split("/", 3)
        if len(parts) == 4 and hmac.compare_digest(parts[2], get_access_token()):
            path = "/" + parts[3]
            request.scope["path"] = path
            request.scope["raw_path"] = path.encode()
            path_token_ok = True
        else:
            return JSONResponse({"error": "invalid token"}, status_code=401)

    # The public HTTPS tunnel only exposes the MCP endpoint and the OAuth
    # surface MCP clients need (discovery, registration, authorize, token).
    # Login, health and disconnect stay unreachable from outside. Unknown
    # paths answer 404 so probes learn nothing.
    if _via_tunnel(request):
        public = (
            path.startswith("/garmin")
            or path.startswith("/.well-known/")
            or path.startswith("/authorize")
            or path in ("/register", "/token")
        )
        if not public:
            return JSONResponse({"detail": "Not Found"}, status_code=404)

    # The MCP endpoint requires the per-install access token
    if path.startswith("/garmin") and not path_token_ok:
        supplied = request.query_params.get("token", "")
        auth = request.headers.get("authorization", "")
        if auth.lower().startswith("bearer "):
            supplied = supplied or auth[7:]
        if not hmac.compare_digest(supplied, get_access_token()):
            return JSONResponse({"error": "invalid token"}, status_code=401)

    return await call_next(request)


# ── HTTPS tunnel (cloudflared quick tunnel), managed from the setup page ──

_TUNNEL_URL_RE = re.compile(r"https://[a-z0-9-]+\.trycloudflare\.com")


class _Tunnel:
    def __init__(self):
        self.proc = None
        self.base_url = None
        self.lock = threading.Lock()
        self.ready = threading.Event()

    def running(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def start(self, host: str, port: int) -> str:
        with self.lock:
            if self.running() and self.base_url:
                return self.base_url
            exe = shutil.which("cloudflared")
            if exe is None:
                raise FileNotFoundError("cloudflared_missing")
            self.base_url = None
            self.ready.clear()
            self.proc = subprocess.Popen(
                [exe, "tunnel", "--url", f"http://{host}:{port}"],
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            )
            threading.Thread(target=self._watch, daemon=True).start()
        if not self.ready.wait(timeout=30) or not self.base_url:
            self.stop()
            raise TimeoutError("Tunnel did not come up within 30s")
        return self.base_url

    def _watch(self):
        for line in self.proc.stdout:
            m = _TUNNEL_URL_RE.search(line)
            if m and not self.base_url:
                self.base_url = m.group(0)
                self.ready.set()
        self.ready.set()  # process exited

    def stop(self):
        with self.lock:
            if self.running():
                self.proc.terminate()
            self.proc = None
            self.base_url = None


_tunnel = _Tunnel()


@app.get("/api/tunnel/status")
def tunnel_status():
    running = _tunnel.running() and _tunnel.base_url
    return {
        "cloudflared": shutil.which("cloudflared") is not None,
        "running": bool(running),
        "url": mcp_url(base=_tunnel.base_url) if running else None,
    }


@app.post("/api/tunnel/start")
def tunnel_start():
    # Target the address the server is actually bound to — on Tailscale-only
    # installs nothing listens on 127.0.0.1.
    host = _bind["host"]
    if host in ("0.0.0.0", "::"):
        host = "127.0.0.1"
    try:
        base = _tunnel.start(host, _bind["port"])
    except FileNotFoundError:
        return JSONResponse({"error": "cloudflared_missing"}, status_code=400)
    except TimeoutError as exc:
        return JSONResponse({"error": str(exc)}, status_code=504)
    return {"url": mcp_url(base=base)}


@app.post("/api/tunnel/stop")
def tunnel_stop():
    _tunnel.stop()
    return {"status": "stopped"}


# ── Minimal OAuth 2.1 server (what claude.ai requires for custom connectors) ──
#
# The "authorization" is simply proving you own this install: the consent page
# asks for the connector's access token, and the issued OAuth access token IS
# that same token, which the /garmin middleware already accepts as Bearer.

_oauth_clients: dict[str, dict] = {}
_oauth_codes: dict[str, dict] = {}


def _public_base(request: Request) -> str:
    if _via_tunnel(request):
        scheme = "https"
    else:
        scheme = request.headers.get("x-forwarded-proto") or request.url.scheme
    host = request.headers.get("host") or f"{HOST}:{PORT}"
    return f"{scheme}://{host}"


@app.get("/.well-known/oauth-protected-resource{_:path}")
def oauth_protected_resource(request: Request, _: str = ""):
    base = _public_base(request)
    return {
        "resource": f"{base}/garmin/",
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
    }


@app.get("/.well-known/oauth-authorization-server{_:path}")
def oauth_authorization_server(request: Request, _: str = ""):
    base = _public_base(request)
    return {
        "issuer": base,
        "authorization_endpoint": f"{base}/authorize",
        "token_endpoint": f"{base}/token",
        "registration_endpoint": f"{base}/register",
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["garmin"],
    }


@app.post("/register")
async def oauth_register(request: Request):
    try:
        body = await request.json()
    except Exception:
        body = {}
    client_id = secrets.token_urlsafe(16)
    _oauth_clients[client_id] = {
        "redirect_uris": body.get("redirect_uris") or [],
    }
    return JSONResponse(
        {
            "client_id": client_id,
            "client_id_issued_at": int(time.time()),
            "redirect_uris": body.get("redirect_uris") or [],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "client_name": body.get("client_name", ""),
        },
        status_code=201,
    )


def _authorize_page(params: dict, error: str = "") -> HTMLResponse:
    fields = "".join(
        f'<input type="hidden" name="{_html.escape(str(k))}" value="{_html.escape(str(v))}" />'
        for k, v in params.items()
    )
    err = f'<p class="err">{error}</p>' if error else ""
    return HTMLResponse(f"""<!DOCTYPE html>
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
</form></div></body></html>""")


@app.get("/authorize")
def oauth_authorize(request: Request):
    p = dict(request.query_params)
    if not p.get("redirect_uri") or not p.get("client_id"):
        return JSONResponse({"error": "invalid_request"}, status_code=400)
    return _authorize_page(p)


@app.post("/authorize")
async def oauth_authorize_submit(request: Request):
    form = dict(await request.form())
    supplied = (form.pop("access_token", "") or "").strip()
    if not hmac.compare_digest(supplied, get_access_token()):
        return _authorize_page(form, error="Invalid access token — try again.")
    code = secrets.token_urlsafe(24)
    _oauth_codes[code] = {
        "client_id": form.get("client_id", ""),
        "redirect_uri": form.get("redirect_uri", ""),
        "code_challenge": form.get("code_challenge", ""),
        "expires": time.time() + 600,
    }
    sep = "&" if "?" in form.get("redirect_uri", "") else "?"
    qs = urlencode({"code": code, **({"state": form["state"]} if form.get("state") else {})})
    return RedirectResponse(f"{form.get('redirect_uri', '')}{sep}{qs}", status_code=302)


@app.post("/token")
async def oauth_token(request: Request):
    form = dict(await request.form())
    grant = form.get("grant_type", "")
    if grant == "refresh_token":
        if not hmac.compare_digest(form.get("refresh_token", ""), get_access_token()):
            return JSONResponse({"error": "invalid_grant"}, status_code=400)
    elif grant == "authorization_code":
        data = _oauth_codes.pop(form.get("code", ""), None)
        if data is None or data["expires"] < time.time():
            return JSONResponse({"error": "invalid_grant"}, status_code=400)
        if data["redirect_uri"] != form.get("redirect_uri", ""):
            return JSONResponse({"error": "invalid_grant"}, status_code=400)
        if data["code_challenge"]:
            verifier = form.get("code_verifier", "")
            digest = hashlib.sha256(verifier.encode()).digest()
            expected = base64.urlsafe_b64encode(digest).rstrip(b"=").decode()
            if not hmac.compare_digest(expected, data["code_challenge"]):
                return JSONResponse({"error": "invalid_grant"}, status_code=400)
    else:
        return JSONResponse({"error": "unsupported_grant_type"}, status_code=400)
    token = get_access_token()
    return JSONResponse(
        {
            "access_token": token,
            "token_type": "Bearer",
            "expires_in": 31536000,
            "refresh_token": token,
            "scope": "garmin",
        },
        headers={"Cache-Control": "no-store"},
    )


# ── Setup page ──

_SETUP_PAGE = resources.files("garmin_mcp").joinpath("setup_page.html").read_text()


@app.get("/")
def root():
    return RedirectResponse("/setup")


@app.get("/setup")
def setup_page():
    return HTMLResponse(_SETUP_PAGE)


@app.get("/health")
def health(request: Request):
    connected = garmin.is_connected()
    payload = {"status": "ok", "connected": connected}
    if connected:
        # Build the MCP URL from the host the client actually used, so the
        # setup page shows a URL that works from that device (localhost, LAN
        # IP or Tailscale IP). An explicit GARMIN_MCP_PUBLIC_URL still wins.
        if os.environ.get("GARMIN_MCP_PUBLIC_URL"):
            payload["mcp_url"] = mcp_url()
        else:
            host = request.headers.get("host") or f"{HOST}:{PORT}"
            payload["mcp_url"] = mcp_url(base=f"http://{host}")
    return payload


@app.post("/api/setup/login")
async def setup_login(request: Request):
    body = await request.json()
    email = (body.get("email") or "").strip()
    password = body.get("password") or ""
    if not email or not password:
        return JSONResponse({"error": "Email and password are required"}, status_code=400)
    try:
        status, session_id = garmin.login(email, password)
    except Exception as exc:
        return JSONResponse({"error": f"Garmin login failed: {exc}"}, status_code=401)
    if status == "needs_mfa":
        return {"mfa_required": True, "session_id": session_id}
    return {"mcp_url": mcp_url()}


@app.post("/api/setup/mfa")
async def setup_mfa(request: Request):
    body = await request.json()
    session_id = body.get("session_id") or ""
    mfa_code = (body.get("mfa_code") or "").strip()
    try:
        garmin.resume_mfa(session_id, mfa_code)
    except ValueError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)
    except Exception as exc:
        return JSONResponse({"error": f"MFA verification failed: {exc}"}, status_code=401)
    return {"mcp_url": mcp_url()}


@app.post("/api/disconnect")
def api_disconnect():
    garmin.disconnect()
    return {"status": "disconnected"}


app.mount("/garmin", mcp.streamable_http_app())


_bind = {"host": HOST, "port": PORT}


def serve(host: str = HOST, port: int = PORT):
    _bind["host"] = host
    _bind["port"] = port
    uvicorn.run(app, host=host, port=port, log_level="info", access_log=True)
