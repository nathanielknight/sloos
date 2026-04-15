"""Playwright integration test for the vanilla JS sloos client.

Spins up a sloos server, a tiny reverse-proxy HTTP server that serves the
test page and proxies `/sloos` to sloos, then drives the page in a real
browser to exercise the full submission flow.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

# Make top-level `tests/conftest.py` importable and find the client/ dir.
ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tests"))

from conftest import Server, read_submissions  # noqa: E402

from proxy_server import build_static_files, make_server  # noqa: E402


@pytest.fixture
def proxied_server(sloos_server: Server):
    static = build_static_files(ROOT / "client")
    server, port, _thread = make_server(sloos_server.base_url, static)
    try:
        yield sloos_server, f"http://127.0.0.1:{port}"
    finally:
        server.shutdown()
        server.server_close()


def _playwright_browser_available() -> bool:
    try:
        from playwright.sync_api import sync_playwright
    except Exception:
        return False
    try:
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            browser.close()
        return True
    except Exception:
        return False


pytestmark = pytest.mark.skipif(
    not _playwright_browser_available(),
    reason="playwright chromium browser is not installed (run `uv run playwright install chromium`)",
)


def test_browser_submits_through_client(proxied_server, page) -> None:
    sloos_server, proxy_url = proxied_server
    page.goto(proxy_url + "/")
    # Wait for sloos() to populate the hidden fields and re-enable the
    # submit button.
    page.wait_for_function(
        "document.querySelector('#f').dataset.sloosReady === '1'",
        timeout=15_000,
    )
    # Hidden fields should be present.
    nonce_value = page.eval_on_selector('input[name="_sloos_nonce"]', "el => el.value")
    pow_value = page.eval_on_selector('input[name="_sloos_pow"]', "el => el.value")
    assert len(nonce_value) == 32
    assert len(pow_value) > 0

    # Submit the form and wait for the fetch to resolve.
    page.click("#submit-btn")
    page.wait_for_function("window.__sloosPostStatus !== undefined", timeout=15_000)
    status = page.evaluate("window.__sloosPostStatus")
    assert status == 200

    # Verify server-side state: the submission should be saved and the
    # callback should have run.
    rows = read_submissions(sloos_server.db_path)
    assert len(rows) == 1
    _id, stored_nonce, data, _ts = rows[0]
    assert stored_nonce == nonce_value
    assert "name=alice" in data
    assert "_sloos_nonce" not in data

    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        if (
            sloos_server.callback_path.exists()
            and sloos_server.callback_secret in sloos_server.callback_path.read_text()
        ):
            break
        time.sleep(0.05)
    assert sloos_server.callback_path.exists()
    assert sloos_server.callback_secret in sloos_server.callback_path.read_text()
