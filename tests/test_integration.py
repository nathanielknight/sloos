"""End-to-end integration tests for the sloos server.

Each test starts a fresh server process (via the `sloos_server` fixture), hits
the HTTP endpoints, then validates both the SQLite database state and that the
configured submit callback actually ran.
"""

import time

from tests.helpers import (
    Server,
    http_get_json,
    http_post_form,
    read_submissions,
    solve_pow,
)


def test_get_returns_well_formed_nonce(sloos_server: Server) -> None:
    resp = http_get_json(sloos_server.base_url + "/")
    assert len(resp["nonce"]) == 32  # 16 bytes hex-encoded
    bytes.fromhex(resp["nonce"])  # raises ValueError if not valid hex
    assert resp["difficulty"] == 8
    assert resp["expires_at"] > 0


def test_post_happy_path(sloos_server: Server) -> None:
    resp = http_get_json(sloos_server.base_url + "/")
    nonce = resp["nonce"]
    difficulty = resp["difficulty"]
    pow_hex = solve_pow(nonce, difficulty)

    status, _body = http_post_form(
        sloos_server.base_url + "/",
        {
            "_sloos_nonce": nonce,
            "_sloos_pow": pow_hex,
            "name": "alice",
            "msg": "hi there",
        },
    )
    assert status == 200

    rows = read_submissions(sloos_server.db_path)
    assert len(rows) == 1
    _id, stored_nonce, data, submitted_at = rows[0]
    assert stored_nonce == nonce
    assert "name=alice" in data
    # Ensure sloos-internal fields aren't persisted in `data`.
    assert "_sloos_nonce" not in data
    assert "_sloos_pow" not in data
    assert submitted_at > 0

    # Wait (briefly) for the async callback to run and check that it echoed
    # the expected secret to the file.
    deadline = time.monotonic() + 5.0
    contents = ""
    while time.monotonic() < deadline:
        if sloos_server.callback_path.exists():
            contents = sloos_server.callback_path.read_text()
            if sloos_server.callback_secret in contents:
                break
        time.sleep(0.05)
    assert sloos_server.callback_secret in contents


def test_post_rejects_replay(sloos_server: Server) -> None:
    resp = http_get_json(sloos_server.base_url + "/")
    nonce = resp["nonce"]
    pow_hex = solve_pow(nonce, resp["difficulty"])
    fields = {"_sloos_nonce": nonce, "_sloos_pow": pow_hex, "x": "1"}
    status, _ = http_post_form(sloos_server.base_url + "/", fields)
    assert status == 200
    status2, _ = http_post_form(sloos_server.base_url + "/", fields)
    assert status2 == 400


def test_post_rejects_bad_pow(sloos_server: Server) -> None:
    resp = http_get_json(sloos_server.base_url + "/")
    nonce = resp["nonce"]
    status, _ = http_post_form(
        sloos_server.base_url + "/",
        {"_sloos_nonce": nonce, "_sloos_pow": "00", "x": "1"},
    )
    assert status == 400
    # Submission should not have been recorded.
    assert read_submissions(sloos_server.db_path) == []


def test_post_rejects_unknown_nonce(sloos_server: Server) -> None:
    status, _ = http_post_form(
        sloos_server.base_url + "/",
        {"_sloos_nonce": "00" * 16, "_sloos_pow": "deadbeef"},
    )
    assert status == 400


def test_post_missing_fields(sloos_server: Server) -> None:
    status, _ = http_post_form(sloos_server.base_url + "/", {"foo": "bar"})
    assert status == 400
