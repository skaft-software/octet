#!/usr/bin/env python3
"""Create an inert local API 0.4 package from static Python tool declarations."""
import argparse, ast, json, os, pathlib, re, tomllib

MAX_SOURCE, MAX_ASSET, MAX_TOTAL = 1_000_000, 10_000_000, 20_000_000
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('source', type=pathlib.Path); p.add_argument('output', type=pathlib.Path)
p.add_argument('--name', required=True); p.add_argument('--version', default='0.1.0')
p.add_argument('--tool', action='append', required=True, help='literal tool name (repeatable)')
p.add_argument('--skill', type=pathlib.Path); p.add_argument('--asset', action='append', type=pathlib.Path, default=[])
a = p.parse_args()

def regular_no_links(raw):
    if '..' in pathlib.Path(raw).parts: raise ValueError(f'parent traversal refused: {raw}')
    path = pathlib.Path(os.path.abspath(raw))
    for part in (path, *path.parents):
        if part.is_symlink(): raise ValueError(f'symlink path refused: {raw}')
    if not path.is_file(): raise ValueError(f'not a regular file: {raw}')
    return path

def toml_string(value): return json.dumps(value, ensure_ascii=True)

try:
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,63}', a.name): raise ValueError('invalid extension name')
    if not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', a.version): raise ValueError('version must be semantic X.Y.Z')
    if len(a.tool) != len(set(a.tool)) or any(not re.fullmatch(r'[a-z][a-z0-9_]{0,63}', n) for n in a.tool): raise ValueError('invalid or duplicate tool name')
    source = regular_no_links(a.source)
    if source.stat().st_size > MAX_SOURCE: raise ValueError('source exceeds 1 MB')
    source_bytes = source.read_bytes(); tree = ast.parse(source_bytes.decode('utf-8'))
    declarations = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)): continue
        for deco in node.decorator_list:
            if not isinstance(deco, ast.Call) or not isinstance(deco.func, ast.Attribute) or deco.func.attr != 'tool': continue
            names = [kw.value for kw in deco.keywords if kw.arg == 'name']
            descriptions = [kw.value for kw in deco.keywords if kw.arg == 'description']
            if len(names) != 1 or not isinstance(names[0], ast.Constant) or not isinstance(names[0].value, str): raise ValueError('every @*.tool declaration needs one literal name= string')
            if len(descriptions) != 1 or not isinstance(descriptions[0], ast.Constant) or not isinstance(descriptions[0].value, str) or not descriptions[0].value.strip(): raise ValueError('every @*.tool declaration needs one non-empty literal description= string')
            if not re.fullmatch(r'[a-z][a-z0-9_]{0,63}', names[0].value): raise ValueError(f'invalid static tool name: {names[0].value!r}')
            declarations.append(names[0].value)
    if len(declarations) != len(set(declarations)): raise ValueError('duplicate @tool declarations')
    if set(declarations) != set(a.tool): raise ValueError(f'--tool declarations must exactly match source: {sorted(declarations)}')
    output = pathlib.Path(os.path.abspath(a.output))
    if '..' in a.output.parts: raise ValueError('parent traversal refused in output path')
    if output.exists() or output.is_symlink(): raise ValueError('output already exists')
    for parent in output.parents:
        if parent.is_symlink(): raise ValueError(f'output ancestor is a symlink: {parent}')
    files = [(source, pathlib.Path('author.py'), source_bytes)]
    sdk = pathlib.Path(__file__).resolve().parents[1] / 'sdk/python/octet_extension'
    sdk_files = sorted(sdk.rglob('*.py'))
    if (sdk.is_symlink() or not sdk.is_dir() or not sdk_files
            or any(path.is_symlink() for path in sdk.rglob('*'))
            or any(not path.is_file() for path in sdk_files)):
        raise ValueError('SDK source tree is missing or contains non-regular files')
    for path in sdk_files:
        files.append((path, pathlib.Path('vendor/octet_extension') / path.relative_to(sdk), path.read_bytes()))
    license_file = pathlib.Path(__file__).resolve().parents[1] / 'LICENSE'
    if license_file.is_symlink() or not license_file.is_file(): raise ValueError('SDK license unavailable')
    files.append((license_file, pathlib.Path('SDK-LICENSE'), license_file.read_bytes()))
    root = pathlib.Path(__file__).resolve().parents[1]
    host_version = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['package']['version']
    sdk_version = tomllib.loads((sdk.parent / 'pyproject.toml').read_text())['project']['version']
    provenance = f'octet-extension-sdk {sdk_version}; source: sdk/python/octet_extension in this octet checkout; license: MIT (SDK-LICENSE).\n'.encode()
    files.append((source, pathlib.Path('SDK-PROVENANCE.txt'), provenance))
    if a.skill:
        skill = regular_no_links(a.skill)
        if skill.stat().st_size > MAX_ASSET: raise ValueError('SKILL.md exceeds 10 MB')
        files.append((skill, pathlib.Path('SKILL.md'), skill.read_bytes()))
    for asset in a.asset:
        path = regular_no_links(asset)
        if path.name.startswith('.') or path.name.lower() in {'node_modules', '.git', '.env'}: raise ValueError(f'hidden/unsafe asset refused: {path.name}')
        if any(ord(char) < 32 or ord(char) == 127 for char in path.name): raise ValueError('asset filename contains control characters')
        path.name.encode('utf-8')
        if path.stat().st_size > MAX_ASSET: raise ValueError(f'asset exceeds 10 MB: {path}')
        files.append((path, pathlib.Path('assets') / path.name, path.read_bytes()))
    rels = [rel for _, rel, _ in files]
    if len(rels) != len(set(rels)): raise ValueError('asset destination collision')
    if sum(len(data) for _, _, data in files) > MAX_TOTAL: raise ValueError('source and assets exceed 20 MB total')
    manifest = (f'name = {toml_string(a.name)}\nversion = {toml_string(a.version)}\napi_version = "0.4"\nrequires_octet = {toml_string("=" + host_version)}\n\n'
        f'[entrypoint]\ncommand = "run-extension"\nargs = []\n\n'
        '[capabilities]\nfilesystem = "none"\nprocess = false\nnetwork = false\n\n'
        f'[contributes]\ntools = [{", ".join(toml_string(n) for n in sorted(a.tool))}]\n')
    tomllib.loads(manifest)
    wrapper = b'import pathlib, runpy, sys\nsys.path.insert(0, str(pathlib.Path(__file__).parent / \"vendor\"))\nrunpy.run_path(str(pathlib.Path(__file__).with_name(\"author.py\")), run_name=\"__main__\")\n'
    launcher = b'#!/bin/sh\nset -eu\ndir=${OCTET_EXTENSION_DIR:-$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)}\nPYTHONDONTWRITEBYTECODE=1 PYTHONPATH=\"$dir/vendor\" exec python3 -B \"$dir/author.py\"\n'
except (ValueError, OSError, UnicodeError, SyntaxError, RecursionError) as error:
    p.error(str(error))

output.mkdir()
for _, rel, data in files:
    dest = output / rel; dest.parent.mkdir(parents=True, exist_ok=True); dest.write_bytes(data)
(output / 'extension.py').write_bytes(wrapper)
(output / 'extension.toml').write_text(manifest, encoding='utf-8')
(output / 'run-extension').write_bytes(launcher)
(output / 'run-extension').chmod(0o755)
print(f'Created local package {output}; review before explicit enablement.')
