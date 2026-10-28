"""Install/upgrade/uninstall smoke tests, exclusively on disposable GitHub runners.

Windows uses a unique /DIR; Linux uses a disposable Docker container. Never run
this tool against a developer installation or an existing user profile.
"""
import argparse
import hashlib
import os
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
from package_build import package_name, sha256
from verify_daemon_crash import verify as verify_crash


def run(argv):
    subprocess.run([str(part) for part in argv], check=True, timeout=300)


def windows(package):
    with tempfile.TemporaryDirectory(prefix='peerlume-install-smoke-') as temporary:
        root = Path(temporary)
        installed = root / 'application'
        profile = root / 'profile'
        payload = root / 'payload'
        profile.mkdir()
        payload.mkdir()
        markers = [profile / 'state.sqlite3', profile / 'user-settings.txt', payload / 'keep.bin']
        for marker in markers:
            marker.write_bytes(b'installer must preserve this fixture\x00')
        expected = {path: hashlib.sha256(path.read_bytes()).digest() for path in markers}
        for _ in range(2):  # Fresh installation, then replacement/upgrade in place.
            run([package, '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', f'/DIR={installed}'])
            for binary in ('peerlume-daemon.exe', 'peerlume-control.exe', 'peerlume-mcp.exe', 'peerlume-tui.exe'):
                run([installed / binary, '--version'])
            assert all(hashlib.sha256(path.read_bytes()).digest() == digest for path, digest in expected.items())
        verify_crash(installed / 'peerlume-daemon.exe',root)
        run([installed / 'unins000.exe', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'])
        assert not (installed / 'peerlume-daemon.exe').exists(), 'Uninstall retained program binary'
        assert all(path.exists() and hashlib.sha256(path.read_bytes()).digest() == digest for path, digest in expected.items()), 'Profile or payload changed'


def linux(package, flavor):
    # No host system package installation. The read-only bind contains only artifacts.
    package_name_deb = 'peerlume' if flavor == 'desktop' else 'peerlume-headless'
    script = r'''
set -eu
export DEBIAN_FRONTEND=noninteractive
mkdir /tmp/profile /tmp/payload
printf 'preserve profile' >/tmp/profile/state.sqlite3
printf 'preserve settings' >/tmp/profile/settings.toml
printf 'preserve payload' >/tmp/payload/data.bin
sha256sum /tmp/profile/* /tmp/payload/* >/tmp/before.sha256
apt-get update -qq
apt-get install -y --no-install-recommends "/evidence/$1"
apt-get install -y --no-install-recommends python3
for binary in peerlume-daemon peerlume-control peerlume-mcp peerlume-tui; do "$binary" --version; done
dpkg -i "/evidence/$1"
sha256sum -c /tmp/before.sha256
python3 /smoke/verify_daemon_crash.py --binary /usr/bin/peerlume-daemon --directory /tmp
dpkg --purge "$2"
test ! -e /usr/bin/peerlume-daemon
sha256sum -c /tmp/before.sha256
'''
    run(['docker', 'run', '--rm', '--network=bridge', '-e', 'GITHUB_ACTIONS=true', '--mount',
         f'type=bind,src={package.parent},dst=/evidence,readonly',
         '--mount', f'type=bind,src={Path(__file__).resolve().parent},dst=/smoke,readonly',
         'ubuntu:24.04', 'sh', '-ec', script, 'installer-smoke', package.name, package_name_deb])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', choices=('windows-x64', 'linux-x64'), required=True)
    parser.add_argument('--flavor', choices=('desktop', 'tui'), required=True)
    parser.add_argument('--commit', required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}',args.commit):
        parser.error('Expected the exact source commit SHA')
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        parser.error('Real installation is permitted only on disposable GitHub Actions runners')
    if {'win32': 'windows-x64', 'linux': 'linux-x64'}.get(sys.platform) != args.platform:
        parser.error('Native operating system mismatch')
    repository = Path(__file__).resolve().parent.parent
    version = tomllib.loads((repository / 'Cargo.toml').read_text())['package']['version']
    name = package_name(version, args.platform, args.commit, args.flavor)
    package = repository / 'dist/builds' / (name + ('-setup.exe' if sys.platform == 'win32' else '.deb'))
    if not package.is_file():
        parser.error('Native package missing')
    checksum=Path(str(package)+'.sha256').read_text(encoding='utf-8').split()[0]
    if sha256(package)!=checksum:
        parser.error('Native package checksum mismatch; refusing execution')
    if sys.platform == 'win32':
        windows(package)
    else:
        linux(package, args.flavor)
    print('Verified native fresh install, replacement upgrade, uninstall and synthetic profile/payload preservation')


if __name__ == '__main__':
    main()
