#!/usr/bin/env python3
"""Loopback-only feed for a disposable Tauri client (never use production keys).

The old test application must embed http://127.0.0.1:18368/update and the public
counterpart of the key that signed --package. Confirm installation in its normal UI.
"""
import argparse
from datetime import datetime, timezone
import http.server
import json
from pathlib import Path
from urllib.parse import quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path, required=True)
    parser.add_argument('--signature', type=Path, required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    package = args.package.read_bytes()
    signature = args.signature.read_text().strip()
    artifact_url = '/' + quote(args.package.name)
    descriptor = json.dumps(dict(version=args.version, notes='Проверка установки выпуска veruna-pack', pub_date=datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z'), url='http://127.0.0.1:18368' + artifact_url, signature=signature), ensure_ascii=False).encode()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == '/update':
                content = descriptor
                content_type = 'application/json'
            elif self.path == artifact_url:
                content = package
                content_type = 'application/octet-stream'
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)

    print(f'Tauri test feed: http://127.0.0.1:18368/update version={args.version} bytes={len(package)}', flush=True)
    http.server.ThreadingHTTPServer(('127.0.0.1', 18368), Handler).serve_forever()


if __name__ == '__main__':
    main()
