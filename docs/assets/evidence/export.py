#!/usr/bin/env python3
"""Package pinned compact historical evidence without running a campaign."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import zipfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def build():
    manifest = json.loads((HERE / 'manifest.json').read_text())
    payload = {}
    for entry in manifest['files']:
        name = entry['path']
        path = ROOT / name
        if Path(name).is_absolute() or '..' in Path(name).parts or path.is_symlink():
            raise ValueError(f'Invalid evidence path: {name}')
        data = path.read_bytes()
        if len(data) != entry['bytes'] or digest(data) != entry['sha256']:
            raise ValueError(f'Pinned historical evidence changed: {name}')
        if name in payload:
            raise ValueError(f'Duplicate evidence path: {name}')
        payload[name] = data
    for name in ['README.md', 'manifest.json', 'export.py']:
        payload[f'docs/assets/evidence/{name}'] = (HERE / name).read_bytes()
    sums = ''.join(f'{digest(data)}  {name}\n' for name, data in sorted(payload.items())).encode()
    payload['SHA256SUMS'] = sums
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w', compression=zipfile.ZIP_STORED) as archive:
        for name, data in sorted(payload.items()):
            info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data)
    package = stream.getvalue()
    return {'historical-evidence.zip': package,
            'historical-evidence.zip.sha256':
                f'{digest(package)}  historical-evidence.zip\n'.encode()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--write', action='store_true')
    mode.add_argument('--check', action='store_true')
    args = parser.parse_args()
    for name, data in build().items():
        path = HERE / name
        if args.write:
            path.write_bytes(data)
        elif path.read_bytes() != data:
            raise ValueError(f'Stale evidence export: {name}')
    print('Pinned compact historical evidence verified; ZIP ' +
          ('written' if args.write else 'reproduced exactly'))


if __name__ == '__main__':
    main()
