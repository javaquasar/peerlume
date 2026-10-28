"""Native packages from the already verified archive; never read a user profile.

No installation, service enablement, association or application launch occurs here.
Windows requires the runner's Inno Setup 6 compiler; Linux uses dpkg-deb rootlessly.
"""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib
from package_build import FLAVORS, package_name, sha256
from verify_build import extract_and_verify


def windows_script(root, output, name, version, flavor):
    if flavor not in FLAVORS or not re.fullmatch(r'[A-Za-z0-9._+-]+', name) or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?', version):
        raise ValueError('Unsafe installer metadata')
    # All values below are generated names or canonical staging paths, not user
    # strings. Inno expands braces, so reject them and quotes in input paths.
    if any(any(char in str(value) for char in '{}"\n\r') for value in (root, output)):
        raise ValueError("Unsafe installer staging path")
    files = sorted(path for path in root.rglob('*') if path.is_file())
    lines = [
        '[Setup]', f'AppId=PeerLume-{flavor}', 'AppName=' + ('PeerLume' if flavor == 'desktop' else 'PeerLume Headless'),
        f'AppVersion={version}', 'AppPublisher=PeerLume contributors',
        'DefaultDirName={localappdata}\\Programs\\' + ('PeerLume' if flavor == 'desktop' else 'PeerLume Headless'),
        'PrivilegesRequired=lowest', 'ArchitecturesAllowed=x64compatible',
        'ArchitecturesInstallIn64BitMode=x64compatible', 'Compression=lzma2',
        'SolidCompression=yes', 'WizardStyle=modern', 'UninstallDisplayName=' + ('PeerLume' if flavor == 'desktop' else 'PeerLume Headless'),
        f'OutputDir={output}', f'OutputBaseFilename={name}-setup',
        'CloseApplications=no', 'RestartApplications=no',
        '[Files]',
    ]
    for path in files:
        if path.is_symlink():
            raise ValueError('Symbolic link in installer input')
        parent = path.relative_to(root).parent
        destination = '{app}' + ('\\' + str(parent) if parent != Path('.') else '')
        lines.append(f'Source: "{path}"; DestDir: "{destination}"; Flags: ignoreversion')
    if flavor == 'desktop':
        lines.extend(['[Tasks]', 'Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked',
                      '[Icons]', 'Name: "{userprograms}\\PeerLume"; Filename: "{app}\\peerlume.exe"',
                      'Name: "{userdesktop}\\PeerLume"; Filename: "{app}\\peerlume.exe"; Tasks: desktopicon'])
    # Deliberately no [Run], [Registry], [UninstallDelete], services or autostart.
    # Neither upgrading nor uninstalling owns downloads, settings or credentials.
    return '\n'.join(lines) + '\n'


def debian_tree(root, stage, repository, version, commit, flavor):
    package = 'peerlume' if flavor == 'desktop' else 'peerlume-headless'
    conflicts = 'peerlume-headless' if flavor == 'desktop' else 'peerlume'
    control = stage / 'DEBIAN'
    control.mkdir(parents=True)
    dependencies = 'libc6 (>= 2.39), libgcc-s1'
    if flavor == 'desktop':
        dependencies += ', libx11-6, libxi6, libxrandr2, libxcb1, libxkbcommon0, libxkbcommon-x11-0, libegl1, libgl1'
    (control / 'control').write_text(
        f'Package: {package}\nVersion: {version}+git{commit[:12]}\nArchitecture: amd64\n'
        'Maintainer: PeerLume contributors <noreply@users.noreply.github.com>\n'
        f'Depends: {dependencies}\nConflicts: {conflicts}\n'
        'Section: net\nPriority: optional\n'
        'Description: PeerLume torrent client with headless session owner\n'
        ' Local CLI/MCP control is opt-in. User data is never bundled or removed.\n', encoding='utf-8')
    for source in sorted(root.rglob('*')):
        if not source.is_file():
            continue
        relative = source.relative_to(root)
        destination = stage / 'usr' / relative if relative.parts[0] in ('bin', 'share') else stage / 'usr/share/doc' / package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o755 if relative.parts[0] == 'bin' else 0o644)
    service = stage / 'usr/lib/systemd/user/peerlume.service'
    service.parent.mkdir(parents=True)
    shutil.copyfile(repository / 'assets/linux/peerlume.service', service)
    for directory in stage.rglob('*'):
        if directory.is_dir():
            directory.chmod(0o755)
    # No maintainer scripts, root daemon, auto-enable or profile deletion.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', choices=('windows-x64', 'linux-x64'), required=True)
    parser.add_argument('--flavor', choices=FLAVORS, default='desktop')
    parser.add_argument('--commit', required=True)
    parser.add_argument('--iscc', help='Absolute path to installed Inno Setup 6 compiler')
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.commit):
        parser.error('Expected a full lowercase source commit SHA')
    repository = Path(__file__).resolve().parent.parent
    version = tomllib.loads((repository / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    name = package_name(version, args.platform, args.commit, args.flavor)
    output = repository / 'dist/builds'
    windows = args.platform == 'windows-x64'
    archive = output / (name + ('.zip' if windows else '.tar.gz'))
    result = output / (name + ('-setup.exe' if windows else '.deb'))
    sidecar = Path(str(result) + '.sha256')
    if result.exists() or sidecar.exists():
        raise ValueError('Native package already exists; never overwrite evidence')
    with tempfile.TemporaryDirectory(prefix='peerlume-native-') as temporary:
        temporary = Path(temporary)
        root = extract_and_verify(archive, args.platform, args.commit, version, temporary, args.flavor)
        if windows:
            candidates = [Path(args.iscc)] if args.iscc else [Path(r'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'), Path(r'C:\Program Files\Inno Setup 6\ISCC.exe')]
            compiler = next((candidate for candidate in candidates if candidate.is_absolute() and candidate.is_file()), candidates[0])
            if not compiler.is_absolute() or not compiler.is_file():
                raise ValueError('Inno Setup 6 compiler unavailable')
            script = temporary / 'installer.iss'
            script.write_text(windows_script(root, output, name, version, args.flavor), encoding='utf-8-sig')
            subprocess.run([str(compiler), str(script)], check=True, timeout=300)
        else:
            stage = temporary / 'debian'
            debian_tree(root, stage, repository, version, args.commit, args.flavor)
            subprocess.run(['dpkg-deb', '--root-owner-group', '--build', str(stage), str(result)], check=True, timeout=300)
            subprocess.run(['dpkg-deb', '--info', str(result)], check=True, timeout=30)
    if not result.is_file():
        raise ValueError('Native compiler did not produce the expected package')
    with sidecar.open('x', encoding='utf-8') as stream:
        stream.write(f'{sha256(result)}  {result.name}\n')
    print(f'Built unsigned native package: {result.name}')


if __name__ == '__main__':
    main()
