"""Reject a fuzz graph which silently loses the transfer engine's local patches."""
from pathlib import Path
import tomllib


def check(root):
    root = Path(root).resolve()
    normal = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
    fuzz = tomllib.loads((root / 'fuzz/Cargo.toml').read_text(encoding='utf-8'))
    for name in ('librqbit', 'librqbit-tracker-comms'):
        expected = (root / normal['patch']['crates-io'][name]['path']).resolve()
        actual = (root / 'fuzz' / fuzz['patch']['crates-io'][name]['path']).resolve()
        if expected != actual:
            raise ValueError(f'Fuzz patch differs from the client engine: {name}')
        packages = tomllib.loads((root / 'fuzz/Cargo.lock').read_text(encoding='utf-8'))['package']
        matches = [package for package in packages if package['name'] == name]
        if len(matches) != 1 or 'source' in matches[0]:
            raise ValueError(f'Fuzz lockfile does not resolve a unique local patch: {name}')


if __name__ == '__main__':
    check(Path(__file__).resolve().parent.parent)
    print('Fuzz and client resolve the same local engine and tracker patches')
