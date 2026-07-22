"""Garmin Connect access through garth. All auth stays on this machine."""

import shutil
import uuid

import garth
import garth.sso

from .config import ACCESS_TOKEN_FILE, TOKENS_DIR

# Garmin's Cloudflare rejects garth's default mobile User-Agent since the
# 2025 auth-flow change; a desktop browser UA passes (login, MFA and resume).
BROWSER_UA = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/131.0.0.0 Safari/537.36"
)

# In-memory MFA continuations: session_id -> garth client state
_mfa_sessions: dict[str, dict] = {}

_resumed = False


class NotConnected(Exception):
    pass


def _apply_ua() -> None:
    garth.client.sess.headers.update({"User-Agent": BROWSER_UA})


def is_connected() -> bool:
    return (TOKENS_DIR / "oauth2_token.json").exists()


def ensure_client() -> None:
    global _resumed
    if not is_connected():
        raise NotConnected(
            "No Garmin account connected. Run `garmin-mcp login` or use the setup page."
        )
    if not _resumed:
        _apply_ua()
        garth.resume(str(TOKENS_DIR))
        _resumed = True


def login(email: str, password: str):
    """Start a Garmin login. Returns ('ok', None) or ('needs_mfa', session_id)."""
    global _resumed
    _apply_ua()
    result = garth.sso.login(
        email, password, client=garth.client, return_on_mfa=True
    )
    if result[0] == "needs_mfa":
        session_id = uuid.uuid4().hex
        _mfa_sessions[session_id] = result[1]
        return "needs_mfa", session_id
    garth.client.oauth1_token, garth.client.oauth2_token = result
    garth.save(str(TOKENS_DIR))
    _resumed = True
    return "ok", None


def resume_mfa(session_id: str, mfa_code: str) -> None:
    global _resumed
    client_state = _mfa_sessions.pop(session_id, None)
    if client_state is None:
        raise ValueError("Unknown or expired MFA session. Sign in again.")
    oauth1, oauth2 = garth.sso.resume_login(client_state, mfa_code)
    garth.client.oauth1_token, garth.client.oauth2_token = oauth1, oauth2
    garth.save(str(TOKENS_DIR))
    _resumed = True


def disconnect() -> None:
    """Forget the Garmin account. The MCP access token is kept on purpose so
    connector URLs pasted into Claude keep working across re-logins; rotate it
    explicitly with `garmin-mcp rotate-token`."""
    global _resumed
    _resumed = False
    shutil.rmtree(TOKENS_DIR, ignore_errors=True)


def api(path: str, **params):
    ensure_client()
    return garth.connectapi(path, params=params or None)


def display_name() -> str:
    profile = api("/userprofile-service/socialProfile")
    return profile["displayName"]
