"""Minimal stdlib HTTP server that:

- serves `index.html` and `sloos.js` from the test page / client dirs
- reverse-proxies any `/sloos` or `/sloos/*` requests to the real sloos server

Used to give the Playwright test a single same-origin endpoint it can target.
"""

from __future__ import annotations

import http.server
import socket
import threading
import urllib.error
import urllib.request
from pathlib import Path


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    # These are filled in by `make_server`.
    static_files: dict[str, tuple[bytes, str]] = {}
    sloos_url: str = ""

    def log_message(self, format: str, *args) -> None:  # pragma: no cover
        return

    def _serve_static(self) -> bool:
        body_ctype = self.static_files.get(self.path)
        if not body_ctype:
            return False
        body, ctype = body_ctype
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        return True

    def _is_sloos(self) -> bool:
        return self.path == "/sloos" or self.path.startswith("/sloos/")

    def _proxy(self, method: str, body: bytes | None) -> None:
        target = self.sloos_url + self.path[len("/sloos") :]
        if target.endswith(""):
            pass
        if not target or target == self.sloos_url:
            target = self.sloos_url + "/"
        headers = {}
        ct = self.headers.get("Content-Type")
        if ct:
            headers["Content-Type"] = ct
        req = urllib.request.Request(target, data=body, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req) as r:
                resp_body = r.read()
                status = r.status
                resp_ctype = r.headers.get("Content-Type", "text/plain")
        except urllib.error.HTTPError as e:
            resp_body = e.read()
            status = e.code
            resp_ctype = e.headers.get("Content-Type", "text/plain")
        self.send_response(status)
        self.send_header("Content-Type", resp_ctype)
        self.send_header("Content-Length", str(len(resp_body)))
        self.end_headers()
        self.wfile.write(resp_body)

    def do_GET(self) -> None:
        if self._is_sloos():
            self._proxy("GET", None)
            return
        if self._serve_static():
            return
        self.send_error(404)

    def do_POST(self) -> None:
        if self._is_sloos():
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length) if length > 0 else b""
            self._proxy("POST", body)
            return
        self.send_error(404)


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def make_server(
    sloos_url: str,
    static_files: dict[str, tuple[bytes, str]],
) -> tuple[http.server.ThreadingHTTPServer, int, threading.Thread]:
    handler = type(
        "BoundProxyHandler",
        (ProxyHandler,),
        {"static_files": static_files, "sloos_url": sloos_url},
    )
    port = _find_free_port()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, port, thread


def build_static_files(client_dir: Path) -> dict[str, tuple[bytes, str]]:
    sloos_js = (client_dir / "sloos.js").read_bytes()
    index_html = b"""<!doctype html>
<html>
<head><meta charset="utf-8"><title>sloos test page</title></head>
<body>
<form id="f" class="sloos-form" method="POST" action="/sloos">
  <input type="text" name="name" value="alice">
  <input type="text" name="msg" value="hi there">
  <button id="submit-btn" type="submit">Send</button>
</form>
<pre id="result"></pre>
<script src="/sloos.js"></script>
<script>
  window.__sloosReady = sloos("/sloos", "#f").then(() => {
    window.__sloosDone = true;
  });
  document.getElementById("f").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const form = ev.target;
    const fd = new FormData(form);
    const body = new URLSearchParams();
    for (const [k, v] of fd.entries()) body.append(k, v);
    const r = await fetch("/sloos", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: body.toString(),
    });
    document.getElementById("result").textContent =
      "status=" + r.status + " " + (await r.text());
    window.__sloosPostStatus = r.status;
  });
</script>
</body>
</html>
"""
    return {
        "/": (index_html, "text/html; charset=utf-8"),
        "/index.html": (index_html, "text/html; charset=utf-8"),
        "/sloos.js": (sloos_js, "application/javascript; charset=utf-8"),
    }
