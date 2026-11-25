#!/usr/bin/env python3
"""Create disposable legacy/CLI copies and one test key pair per product.

Usage: python3 scripts/prepare-product-copies.py /tmp/veruna-pack-rehearsal
Requires Node 22.12+, npm and Git access to both source repositories.
Only a new directory is accepted; existing work is never overwritten.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess


def run(*args, cwd=None, quiet=False):
    return subprocess.run(args, cwd=cwd, check=True, stdout=subprocess.PIPE if quiet else None, stderr=subprocess.PIPE if quiet else None)


def commit(repo, message):
    run('git', 'add', '--all', cwd=repo)
    run('git', '-c', 'commit.gpgsign=false', 'commit', '-m', message, cwd=repo)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('work', type=Path)
    parser.add_argument('--mockport-source', default='git@github.com:averichev/mockport.git')
    parser.add_argument('--host-source', default='git@github.com:averichev/host-util.git')
    parser.add_argument('--base-version', default='0.50.0')
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=False)
    keys = work / 'test-keys'
    keys.mkdir(mode=0o700)
    utility = Path(__file__).resolve().parent.parent
    mock = work / 'mockport'
    host = work / 'host-util'
    run('git', 'clone', '--no-hardlinks', args.mockport_source, str(mock))
    run('git', 'clone', '--no-hardlinks', args.host_source, str(host))
    js = """const fs=require('node:fs'),c=require('node:crypto');
const {privateKey,publicKey}=c.generateKeyPairSync('ed25519');
fs.writeFileSync(process.argv[1],privateKey.export({type:'pkcs8',format:'pem'}),{mode:0o600});
fs.writeFileSync(process.argv[2],JSON.stringify(publicKey.export({format:'jwk'})));"""
    run('node', '-e', js, str(keys / 'mockport.pem'), str(keys / 'mockport.jwk'))
    identity_path = mock / 'release.config.json'
    identity = json.loads(identity_path.read_text())
    identity['updates'].update(public_key=json.loads((keys / 'mockport.jwk').read_text()), manifest_url='http://127.0.0.1:18367/manifest.json', descriptor_url='http://127.0.0.1:18367/api/v1/updates/standalone/test/{target}', allowed_download_origins=['http://127.0.0.1:18367'])
    identity_path.write_text(json.dumps(identity, ensure_ascii=False, indent=2) + '\n')
    run('npm', 'ci', cwd=host / 'apps/desktop')
    # Tauri prints key material during generation: capture it, never send to logs.
    run('npm', 'exec', '--', 'tauri', 'signer', 'generate', '--ci', '--password', '', '--write-keys', str(keys / 'host-util.key'), cwd=host / 'apps/desktop', quiet=True)
    os.chmod(keys / 'host-util.key', 0o600)
    path = host / 'apps/desktop/src-tauri/tauri.conf.json'
    config = json.loads(path.read_text())
    config['plugins']['updater'].update(pubkey=(keys / 'host-util.key.pub').read_text().strip(), endpoints=['http://127.0.0.1:18368/update'], dangerousInsecureTransportProtocol=True)
    path.write_text(json.dumps(config, ensure_ascii=False, indent=2) + '\n')
    for repo in [mock, host]:
        commit(repo, 'test: disposable updater key and loopback endpoint')
        run('git', 'tag', 'v' + args.base_version, cwd=repo)
    for source, name, example, obsolete in [
        (mock, 'mockport-next', None, ['scripts/release.mjs', 'scripts/release-lib.mjs', 'scripts/release.sh', 'scripts/build.mjs']),
        (host, 'host-next', 'moy-host.release.toml', ['scripts/release.py', 'scripts/build_macos.py', 'scripts/collect_release.py', 'Dockerfile.release']),
    ]:
        repo = work / name
        run('git', 'clone', '--no-hardlinks', str(source), str(repo))
        if example is not None:
            text = (utility / 'examples' / example).read_text()
            text = text.replace('~/.tauri/host-util.key', str(keys / 'host-util.key'))
            (repo / 'release.toml').write_text(text)
        else:
            (repo / 'release.toml').unlink(missing_ok=True)
        (repo / 'Makefile').write_text((utility / 'examples/Makefile').read_text())
        for relative in obsolete:
            (repo / relative).unlink()
        commit(repo, 'feat(release): проверить выпуск через veruna-pack')
    q = shlex.quote
    print('\nDisposable copies ready; original UUID/identifier preserved. Run:')
    print(f'(cd {q(str(mock))} && MOCKPORT_SIGNING_KEY={q(str(keys / "mockport.pem"))} MOCKPORT_PUBLIC_KEY={q(str(keys / "mockport.jwk"))} make release)')
    print(f'(cd {q(str(host))} && TAURI_SIGNING_PRIVATE_KEY={q(str(keys / "host-util.key"))} TAURI_SIGNING_PRIVATE_KEY_PASSWORD=\'\' make release-macos)')
    for name in ['mockport-next', 'host-next']:
        signing = f'MOCKPORT_SIGNING_KEY={q(str(keys / "mockport.pem"))}' if name == 'mockport-next' else f'TAURI_SIGNING_PRIVATE_KEY={q(str(keys / "host-util.key"))} TAURI_SIGNING_PRIVATE_KEY_PASSWORD=\'\''
        print(f'(cd {q(str(work / name))} && make release-plan && {signing} make release)')


if __name__ == '__main__':
    main()
