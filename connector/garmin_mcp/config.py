import os
import secrets
from pathlib import Path

CONFIG_DIR = Path(os.environ.get("GARMIN_MCP_HOME", Path.home() / ".config" / "garmin-mcp"))
TOKENS_DIR = CONFIG_DIR / "tokens"
ACCESS_TOKEN_FILE = CONFIG_DIR / "access_token"

HOST = os.environ.get("GARMIN_MCP_HOST", "127.0.0.1")
PORT = int(os.environ.get("GARMIN_MCP_PORT", "8765"))

# Origins allowed to call the setup API from a browser (the GitHub Pages site
# and local development). Extend with GARMIN_MCP_EXTRA_ORIGINS (comma-separated).
ALLOWED_ORIGINS = [
    "https://claude.ai",
    "https://www.claude.ai",
    "http://localhost:8765",
    "http://127.0.0.1:8765",
]
_extra = os.environ.get("GARMIN_MCP_EXTRA_ORIGINS", "")
ALLOWED_ORIGINS += [o.strip() for o in _extra.split(",") if o.strip()]


def get_access_token() -> str:
    """Return the persistent MCP access token, creating it on first use."""
    if ACCESS_TOKEN_FILE.exists():
        return ACCESS_TOKEN_FILE.read_text().strip()
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    token = secrets.token_urlsafe(24)
    ACCESS_TOKEN_FILE.write_text(token)
    ACCESS_TOKEN_FILE.chmod(0o600)
    return token


def mcp_url(base: str | None = None) -> str:
    base = (base or os.environ.get("GARMIN_MCP_PUBLIC_URL") or "").rstrip("/")
    if not base:
        base = f"http://{HOST}:{PORT}"
    return f"{base}/garmin/?token={get_access_token()}"
