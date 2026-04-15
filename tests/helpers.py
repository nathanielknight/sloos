"""Shared test utilities for sloos integration tests."""

import hashlib
import json
import sqlite3
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
import subprocess


@dataclass
class Server:
    proc: subprocess.Popen
    base_url: str
    db_path: Path
    callback_path: Path
    callback_secret: str


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
            count += 8 - b.bit_length()
            break
    return count


def http_get_json(url: str) -> dict:
    with urllib.request.urlopen(url) as r:
        body = r.read().decode("utf-8")
    return json.loads(body)


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


def read_submissions(db_path: Path) -> list[tuple[int, str, str, int]]:
    conn = sqlite3.connect(db_path)
    try:
        rows = conn.execute(
            "SELECT id, nonce, data, submitted_at FROM submissions ORDER BY id"
        ).fetchall()
    finally:
        conn.close()
    return rows
