"""Central configuration for uploader app.

Expose a single source of truth for the base site URL and related settings.
"""
from typing import Optional
import secrets

# Default base site URL (can be overridden via CLI or GUI at runtime)
BASE_SITE_URL: str = "https://www.penracourses.org.uk"

# Production URL constant
PROD_BASE_SITE_URL: str = "https://www.penracourses.org.uk"

# Development URL constant (local test server)
DEV_BASE_SITE_URL: str = "http://localhost:8080"

# Optional anonymizer seed to make anonymisation deterministic across processes
ANON_SEED: int | None = None

def set_base_site_url(url: str) -> None:
    global BASE_SITE_URL
    BASE_SITE_URL = url

def get_base_site_url() -> str:
    return BASE_SITE_URL

def init_from_cli(value: Optional[str], debug: bool = False, seed: Optional[int] = None) -> None:
    """Initialize config values from command line arguments.

    If `value` is provided it overrides the default. If `debug` is True
    the base url is set to the local test server.
    """
    global BASE_SITE_URL
    if debug:
        BASE_SITE_URL = DEV_BASE_SITE_URL
    elif value:
        BASE_SITE_URL = value

    # initialize anonymizer seed if provided
    if seed is not None:
        global ANON_SEED
        ANON_SEED = int(seed)

def set_anonymizer_seed(seed: Optional[str]) -> None:
    global ANON_SEED
    ANON_SEED = secrets.token_urlsafe(12) if seed is None else seed

def get_anonymizer_seed() -> str | None:
    return ANON_SEED
