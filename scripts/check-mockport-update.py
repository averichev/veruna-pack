#!/usr/bin/env python3
"""Install a veruna-pack binary through MockPort's production Linux update client.

Both binaries must embed the same disposable key and the loopback descriptor
http://127.0.0.1:18367/api/v1/updates/standalone/test/{target}.
The macOS entry point runs everything in a network-isolated Docker container.
"""
import argparse
import hashlib
import http.cookiejar
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def wait(function, predicate=lambda value: bool(value), seconds=180):
    until = time.monotonic() + seconds
    last = None
    while time.monotonic() < until:
        try:
            last = function()
            if predicate(last):
                return last
        except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
            last = str(error)
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {last}")


def inside(args):
    payload = Path(args.new).read_bytes()
    signed = Path(args.jws).read_text().strip()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if self.path.startswith('/api/v1/updates/standalone/test/'):
                data = json.dumps(dict(version=args.new_version, url='http://127.0.0.1:18367/binary', signed_metadata=signed)).encode()
            elif self.path == '/manifest.json':
                data = json.dumps(dict(schema_version=2, releases=[dict(version=args.new_version, notes='Проверка veruna-pack', artifacts=[dict(purpose='executable', target='linux-x86_64', download_url='http://127.0.0.1:18367/binary')])])).encode()
            elif self.path == '/binary':
                data = payload
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Content-Type', 'application/octet-stream' if self.path == '/binary' else 'application/json')
            self.end_headers()
            self.wfile.write(data)

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 18367), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='veruna-pack-installed-') as temporary:
        root = Path(temporary)
        binary = root / 'mockport'
        shutil.copy2(args.old, binary)
        binary.chmod(0o755)
        env = {key: value for key, value in os.environ.items() if not key.startswith('MOCKPORT_')}
        env.update(PORT='18369', MOCKPORT_DATA_DIR=str(root / 'data'), MOCKPORT_DATABASE='test.sqlite', MOCKPORT_HOST='127.0.0.1', MOCKPORT_UPDATE_MODE='standalone', MOCKPORT_LOG_LEVEL='warn')
        with (root / 'process.log').open('wb') as log:
            subprocess.run([str(binary), 'admin', 'bootstrap', '--stdin'], cwd=root, env=env, input=b'admin\nveruna-pack-test-password\n', stdout=log, stderr=log, check=True)
            process = subprocess.Popen([str(binary), '--no-browser'], cwd=root, env=env, stdout=log, stderr=log)
            client = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            csrf = None

            def api(path, body=None):
                headers = {'X-MockPort-Request': '1'}
                if csrf:
                    headers['X-CSRF-Token'] = csrf
                if body is not None:
                    headers['Content-Type'] = 'application/json'
                request = urllib.request.Request('http://127.0.0.1:18369' + path, headers=headers, data=None if body is None else json.dumps(body).encode())
                with client.open(request, timeout=10) as response:
                    value = response.read()
                    return json.loads(value) if value else None

            try:
                assert wait(lambda: api('/health'))['version'] == args.old_version
                csrf = api('/api/auth/login', dict(username='admin', password='veruna-pack-test-password'))['csrf_token']
                api('/api/admin/systems', dict(slug='preserved', name='Preserve SQLite'))
                api('/api/admin/system/update/check', {})
                checked = wait(lambda: api('/api/admin/system/update/status'), lambda value: value['status']['state'] == 'available')
                assert checked['current_version'] == args.old_version
                api('/api/admin/system/update/install', dict(check_id=checked['status']['operation_id'], version=args.new_version))
                result = wait(lambda: api('/api/admin/system/update/status'), lambda value: value['status']['state'] in ['completed', 'failed', 'rolled_back'])
                assert result['status']['state'] == 'completed', result
                assert result['current_version'] == args.new_version
                assert api('/health')['version'] == args.new_version
                assert digest(binary) == digest(args.new)
                assert api('/api/admin/systems')[0]['name'] == 'Preserve SQLite'
                with client.open('http://127.0.0.1:18369/systems/1/endpoints', timeout=10) as response:
                    html = response.read().decode()
                assert '<title>МокПорт' in html
                asset = html.split('src="')[1].split('"')[0]
                with client.open('http://127.0.0.1:18369' + asset, timeout=10) as response:
                    assert len(response.read()) > 1000
                print(json.dumps(dict(result='passed', client='MockPort/standalone-updater', old_version=args.old_version, new_version=args.new_version, installed_sha256=digest(binary), sqlite='preserved'), ensure_ascii=False))
            except BaseException:
                log.flush()
                print((root / 'process.log').read_text(errors='replace')[-6000:], file=sys.stderr)
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=30)
    server.shutdown()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ['old', 'new', 'jws', 'old-version', 'new-version']:
        parser.add_argument('--' + option, required=True)
    parser.add_argument('--inside', action='store_true')
    args = parser.parse_args()
    if args.inside:
        inside(args)
    else:
        command = ['docker', 'run', '--rm', '--platform', 'linux/amd64', '--network', 'none']
        for source, dest in [(args.old, '/old'), (args.new, '/new'), (args.jws, '/update.jws'), (__file__, '/check.py')]:
            command += ['--mount', f'type=bind,src={Path(source).resolve()},dst={dest},readonly']
        subprocess.run(command + ['python:3.12-alpine', 'python3', '/check.py', '--inside', '--old', '/old', '--new', '/new', '--jws', '/update.jws', '--old-version', args.old_version, '--new-version', args.new_version], check=True)


if __name__ == '__main__':
    main()
