#!/usr/bin/env python3
"""Read-only, loopback-only host for the synthetic graph UI studies."""
import argparse
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent
PAGES = {"index.html", "control-room.html", "route-lanes.html", "patch-bay.html", "tenant-atlas.html", "change-desk.html", "app.js", "styles.css"}
ASSETS = {"manrope.ttf", "serif.ttf", "mono.ttf", "condensed.ttf", "manrope-OFL.txt", "instrumentserif-OFL.txt", "ibmplexmono-OFL.txt", "barlowcondensed-OFL.txt", "control-room.png", "route-lanes.png", "patch-bay.png", "tenant-atlas.png", "change-desk.png", "favicon.svg"}

class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def send_head(self):
        path = unquote(urlsplit(self.path).path)
        if path == "/":
            path = "/index.html"
        if path == "/favicon.ico":
            path = "/assets/favicon.svg"
        relative = path.lstrip("/")
        allowed = relative in PAGES or relative in {"assets/" + name for name in ASSETS}
        file = ROOT / relative
        if not allowed or not file.is_file() or file.is_symlink() or not file.resolve().is_relative_to(ROOT):
            self.send_error(404, "Not found")
            return None
        self.path = path
        return super().send_head()

    def end_headers(self):
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; connect-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
        super().end_headers()

    def log_message(self, format, *args):
        # Do not persist caller-controlled URLs, queries, headers or body contents.
        pass

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=5280)
    args = parser.parse_args()
    print(f"Synthetic graph mockups: http://127.0.0.1:{args.port}/", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()
