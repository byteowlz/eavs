#!/usr/bin/env python3
"""Read-only loopback demo host; serves only built public UI artifacts."""
import argparse
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent / 'dist'
PAGES = {'index.html', 'embedded.html', 'demo.css', 'demo.js', 'control-room.js', 'react.js', 'build-manifest.json', 'react-proof.js', 'react-proof.html'}
ASSETS = {'manrope.ttf', 'mono.ttf', 'manrope-OFL.txt', 'ibmplexmono-OFL.txt', 'favicon.svg', 'react-LICENSE.txt', 'react-dom-LICENSE.txt'}

class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def send_head(self):
        path = unquote(urlsplit(self.path).path)
        if path == '/':
            path = '/index.html'
        if path == '/favicon.ico':
            path = '/assets/favicon.svg'
        name = path.lstrip('/')
        file = ROOT / name
        if not (name in PAGES or name in {'assets/' + asset for asset in ASSETS}) or not file.is_file() or file.is_symlink() or not file.resolve().is_relative_to(ROOT):
            self.send_error(404, 'Not found')
            return None
        self.path = path
        return super().send_head()

    def end_headers(self):
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Referrer-Policy', 'no-referrer')
        self.send_header('Cache-Control', 'no-cache')
        self.send_header('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; connect-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
        super().end_headers()

    def log_message(self, format, *args):
        pass  # Never persist caller-controlled URLs, headers, query strings or bodies.

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=5281)
    args = parser.parse_args()
    print(f'Control Room demo: http://127.0.0.1:{args.port}/', flush=True)
    ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()
