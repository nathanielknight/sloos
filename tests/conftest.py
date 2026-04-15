"""Shared pytest utilities for sloos integration tests."""

from __future__ import annotations

import hashlib
import os
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_port(host: str, port: int, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"server did not start on {host}:{port}")


@dataclass
class Server:
    proc: subprocess.Popen
    base_url: str
    db_path: Path
    callback_path: Path
    callback_secret: str


def build_server() -> Path:
    """Build sloos in release mode and return the binary path."""
    subprocess.run(
        ["cargo", "build", "--release"],
        cwd=REPO_ROOT,
        check=True,
    )
    binary = REPO_ROOT / "target" / "release" / "sloos"
    if not binary.exists():
        raise RuntimeError(f"built binary missing: {binary}")
    return binary


@pytest.fixture(scope="session")
def sloos_binary() -> Path:
    return build_server()


@pytest.fixture
def sloos_server(sloos_binary: Path):
    """Start a fresh sloos server for a test and tear it down afterwards."""
    tmp = tempfile.TemporaryDirectory()
    tmpdir = Path(tmp.name)
    db_path = tmpdir / "sloos.db"
    callback_path = tmpdir / "callback.txt"
    callback_secret = "cb-secret-" + os.urandom(4).hex()
    # Static command — no submission data is passed, just echo a known value.
    callback_cmd = f"echo {callback_secret} >> {callback_path}"
    port = _find_free_port()
    env = {
        **os.environ,
        "SLOOS_DB_PATH": str(db_path),
        "SLOOS_POW_DIFFICULTY": "8",
        "SLOOS_NONCE_EXPIRATION_SECONDS": "60",
        "SLOOS_SUBMIT_CALLBACK": callback_cmd,
        "SLOOS_BIND_ADDR": f"127.0.0.1:{port}",
        "RUST_LOG": "info",
    }
    proc = subprocess.Popen(
        [str(sloos_binary)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        _wait_for_port("127.0.0.1", port)
        yield Server(
            proc=proc,
            base_url=f"http://127.0.0.1:{port}",
            db_path=db_path,
            callback_path=callback_path,
            callback_secret=callback_secret,
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        tmp.cleanup()


def solve_pow(nonce_hex: str, difficulty: int) -> str:
    """Find a PoW hex string for the given nonce and difficulty."""
    nonce_bytes = bytes.fromhex(nonce_hex)
    counter = 0
    while True:
        pow_bytes = counter.to_bytes(8, "big")
        digest = hashlib.sha256(nonce_bytes + pow_bytes).digest()
        if _leading_zero_bits(digest) >= difficulty:
            return pow_bytes.hex()
        counter += 1


def _leading_zero_bits(data: bytes) -> int:
    count = 0
    for b in data:
        if b == 0:
            count += 8
        else:
            # bit length of b: leading zeros = 8 - bit_length
            count += 8 - b.bit_length()
            break
    return count


def http_get_json(url: str) -> dict:
    with urllib.request.urlopen(url) as r:
        body = r.read().decode("utf-8")
    return _parse_nonce_json(body)


def http_post_form(url: str, fields: dict[str, str]) -> tuple[int, str]:
    data = "&".join(
        f"{urllib.parse.quote_plus(k)}={urllib.parse.quote_plus(v)}"
        for k, v in fields.items()
    ).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/x-www-form-urlencoded"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8")


def _parse_nonce_json(body: str) -> dict:
    """Minimal parser for our known response shape. We control the format on
    the server side (no serde), so the format is stable:
    {"nonce":"<hex>","difficulty":<int>,"expires_at":<int>}
    """
    import re

    m = re.fullmatch(
        r'\{"nonce":"([0-9a-f]+)","difficulty":(\d+),"expires_at":(\d+)\}',
        body.strip(),
    )
    if not m:
        raise ValueError(f"unexpected response body: {body!r}")
    return {
        "nonce": m.group(1),
        "difficulty": int(m.group(2)),
        "expires_at": int(m.group(3)),
    }


def read_submissions(db_path: Path) -> list[tuple[int, str, str, int]]:
    conn = sqlite3.connect(db_path)
    try:
        rows = conn.execute(
            "SELECT id, nonce, data, submitted_at FROM submissions ORDER BY id"
        ).fetchall()
    finally:
        conn.close()
    return rows
