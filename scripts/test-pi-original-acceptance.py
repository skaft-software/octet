#!/usr/bin/env python3
"""Offline reviewed-original probes; no native build/install/network/dependencies.
Explicit entrypoints only. Receipts are exclusively created, retaining red runs.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('label')
p.add_argument('--clm')
p.add_argument('--rainbow')
p.add_argument('--footer')
p.add_argument('--termdraw')
a = p.parse_args()
if not a.label.replace('-', '').isalnum():
    p.error('label must be alphanumeric/hyphens')
out = ROOT / 'artifacts/takeover/originals' / a.label
out.mkdir(parents=True, exist_ok=False)
home = out / 'home'
home.mkdir()
node = shutil.which('node')
env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'USERPROFILE': str(home), 'TMPDIR': str(home), 'PI_OFFLINE': '1'}
for flag in ['clm', 'rainbow', 'footer', 'termdraw']:
    if getattr(a, flag):
        env['PI_' + flag.upper() + '_PATH'] = str(Path(getattr(a, flag)).resolve(strict=True))

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def inputs():
    paths = list((ROOT / 'extensions/octet-pi-compat').rglob('*')) + list((ROOT / 'scripts').glob('test-pi-original-acceptance.*'))
    for flag in ['clm', 'footer', 'termdraw']:
        if getattr(a, flag):
            path = Path(getattr(a, flag)).resolve()
            root = path.parent
            while not (root / 'package.json').exists() and root != root.parent:
                root = root.parent
            if root != root.parent:
                paths += list(root.rglob('*'))
    if a.rainbow:
        paths.append(Path(a.rainbow))
    return {
        str(path): digest(path) for path in sorted(set(paths)) if path.is_file() and
        # Exclude nested dependencies, but retain the explicitly selected package
        # under the user's outer node_modules. Lock + adapter runtime pins included.
        str(path).count('/node_modules/') <= (1 if str(path).startswith('/Users/') and not str(path).startswith(str(ROOT)) else 0)
        and '.git' not in path.parts and path.suffix in {'.mjs', '.cjs', '.ts', '.tsx', '.js', '.json', '.py', '.toml'}
    }

tests = []
if a.clm:
    tests.append('extensions/octet-pi-compat/test/original-acceptance-clm.test.mjs')
if a.termdraw:
    tests.append('extensions/octet-pi-compat/test/original-acceptance-termdraw.test.mjs')
if a.rainbow or a.footer:
    tests.append('extensions/octet-pi-compat/test/originals.test.mjs')
if not tests:
    p.error('supply at least one reviewed original entrypoint')
before = inputs()
command = [node, '--test', '--test-concurrency=1', *tests]
start = time.time()
with (out / 'test.log').open('xb') as log:
    result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
after = inputs()
receipt = {'scope': 'synthetic-host unchanged original factories ONLY; no native/PTY/durable-provider qualification', 'command': command, 'cwd': str(ROOT), 'environment': env, 'exit_code': result.returncode, 'seconds': time.time()-start, 'source_before': before, 'source_after': after, 'source_stable': before == after, 'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(), 'node': subprocess.check_output([node, '--version'], text=True).strip()}
(out / 'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
(out / 'exit').write_text(str(result.returncode)+'\n')
print(json.dumps({'receipt': str(out / 'receipt.json'), 'exit': result.returncode, 'source_stable': before == after}))
raise SystemExit(result.returncode or (2 if before != after else 0))
