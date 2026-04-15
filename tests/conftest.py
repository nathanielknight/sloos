"""Shared pytest fixtures for sloos integration tests."""

import os
import secrets
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

from tests.helpers import Server

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
    callback_secret = "cb-secret-" + secrets.token_hex()
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



