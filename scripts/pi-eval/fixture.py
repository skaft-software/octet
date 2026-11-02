#!/usr/bin/env python3
"""Build and cache the production-shaped Pi home used by the Pi extensions eval.

Layout (all under the fixture home, which legs copy to a fresh HOME):

    .pi/agent/install/current-version                 "1.4.0"
    .pi/agent/install/releases/1.4.0/node_modules/@earendil-works/pi-coding-agent
    .pi/agent/npm/{package.json,.gitignore,node_modules/<packages>}
    .pi/agent/pi-eval/pi-eval-sentinel.ts             readiness sentinel
    .pi/agent/settings.json                           packages + extensions + default model

The packages are installed the way Pi's own package manager does it
(dist/core/package-manager.js `ensureNpmProject` + `installNpmBatch` /
`getNpmInstallArgs`): npm prefix `<agentDir>/npm`, project `{name:
"pi-extensions", private: true}`, and `npm install <specs...> --prefix <root>
--legacy-peer-deps`. We add only `--ignore-scripts --no-audit --no-fund`, so no
package lifecycle script runs during the build.

The built home is cached under `<cache>/<lock-hash>/`; the hash covers the
lockfile, the sentinel, the probe and this file's layout version.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
LAYOUT_VERSION = "1"
LOCK_PATH = HERE / "packages.lock.json"
SENTINEL_SOURCE = HERE / "sentinel" / "pi-eval-sentinel.ts"
PROBE_SCRIPT = HERE / "pi_probe.mjs"
SENTINEL_SETTING = "pi-eval/pi-eval-sentinel.ts"  # relative to the agent dir, as Pi resolves it
PROVIDER = "mock"
MODEL = "mock-1"
NPM_EXTRA_FLAGS = ["--ignore-scripts", "--no-audit", "--no-fund"]


def default_cache() -> Path:
    override = os.environ.get("PI_EVAL_CACHE")
    if override:
        return Path(override)
    base = os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    return Path(base) / "octet-pi-eval"


def load_lock(path: Path = LOCK_PATH) -> dict:
    lock = json.loads(Path(path).read_text())
    if lock.get("schema") != "octet.pi-eval-lock/v1":
        raise SystemExit(f"{path}: unsupported lock schema {lock.get('schema')!r}")
    return lock


def lock_hash(lock: dict) -> str:
    digest = hashlib.sha256()
    digest.update(LAYOUT_VERSION.encode())
    digest.update(json.dumps(lock, sort_keys=True).encode())
    for path in (SENTINEL_SOURCE, PROBE_SCRIPT):
        digest.update(path.read_bytes())
    return digest.hexdigest()[:16]


def package_specs(lock: dict) -> list[str]:
    return [f"{entry['name']}@{entry['version']}" for entry in lock["packages"]]


def settings_document(lock: dict) -> dict:
    return {
        "packages": [f"npm:{spec}" for spec in package_specs(lock)],
        "extensions": [SENTINEL_SETTING],
        "defaultProvider": PROVIDER,
        "defaultModel": MODEL,
        "enableInstallTelemetry": False,
        # Suppress the what's-new block so both targets start from the same screen.
        "lastChangelogVersion": lock["pi"]["version"],
    }


@dataclass
class Fixture:
    root: Path
    lock: dict

    @property
    def home(self) -> Path:
        return self.root / "home"

    @property
    def agent(self) -> Path:
        return self.home / ".pi" / "agent"

    @property
    def release(self) -> Path:
        return self.agent / "install" / "releases" / self.lock["pi"]["version"]

    @property
    def pi_cli(self) -> Path:
        return self.release / "node_modules" / "@earendil-works" / "pi-coding-agent" / "dist" / "bundle" / "cli.js"

    @property
    def meta_path(self) -> Path:
        return self.root / "fixture.json"

    @property
    def probe_path(self) -> Path:
        return self.root / "pi-probe.json"

    def meta(self) -> dict:
        return json.loads(self.meta_path.read_text())


def _run(command: list[str], *, cwd: Path, log, timeout: int, env: dict | None = None) -> subprocess.CompletedProcess:
    log(f"$ {' '.join(command)}   (cwd={cwd})")
    started = time.monotonic()
    result = subprocess.run(
        command, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
    )
    log(f"  exit {result.returncode} in {time.monotonic() - started:.1f}s")
    return result


def _npm_env() -> dict:
    env = {key: value for key, value in os.environ.items() if not key.startswith(("PI_", "OCTET_"))}
    env["npm_config_update_notifier"] = "false"
    return env


def copy_tree(source: Path, destination: Path) -> None:
    """Copy a home tree, preserving symlinks and using reflinks where the filesystem has them."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    if shutil.which("cp"):
        result = subprocess.run(
            ["cp", "-a", "--reflink=auto", str(source), str(destination)], capture_output=True, text=True
        )
        if result.returncode == 0:
            return
    shutil.copytree(source, destination, symlinks=True)


def _semver_satisfies_batch(release: Path, checks: list[tuple[str, str]]) -> list[bool]:
    """Evaluate (version, range) pairs with the `semver` package Pi ships (or npm's)."""
    script = (
        "const {createRequire}=require('module');"
        "const dirs=[process.argv[1]+'/node_modules/@earendil-works/pi-coding-agent/', process.argv[1]+'/', require('child_process').execFileSync('npm',['root','-g'],{encoding:'utf8'}).trim()+'/npm/'];"
        "let semver;for(const d of dirs){try{semver=createRequire(d).resolve('semver')&&createRequire(d)('semver');break}catch{}}"
        "if(!semver)throw new Error('semver not found');"
        "const input=JSON.parse(require('fs').readFileSync(0,'utf8'));"
        "console.log(JSON.stringify(input.map(([v,r])=>{try{return semver.satisfies(v,r,{includePrerelease:true})}catch{return true}})));"
    )
    result = subprocess.run(
        ["node", "-e", script, str(release)], input=json.dumps(checks), capture_output=True, text=True, timeout=60
    )
    if result.returncode != 0:
        raise RuntimeError(f"semver check failed: {result.stderr.strip()[:500]}")
    return json.loads(result.stdout)


def compute_peer_exclusions(fixture: Fixture) -> dict:
    """Informational: locked packages whose pi peer ranges exclude the installed Pi."""
    sys.path.insert(0, str(HERE))
    from core import peer_excluded  # local import keeps fixture.py importable standalone

    modules = fixture.agent / "npm" / "node_modules"
    pi_versions = {}
    for name in ("pi-coding-agent", "pi-ai", "pi-tui", "pi-agent-core"):
        manifest = fixture.release / "node_modules" / "@earendil-works" / name / "package.json"
        if manifest.exists():
            pi_versions[f"@earendil-works/{name}"] = json.loads(manifest.read_text())["version"]
    manifests = {}
    for entry in fixture.lock["packages"]:
        path = modules / entry["name"] / "package.json"
        if path.exists():
            manifests[entry["name"]] = json.loads(path.read_text())
    pending: list[tuple[str, str]] = []
    for manifest in manifests.values():
        for peer, wanted in (manifest.get("peerDependencies") or {}).items():
            if peer in pi_versions:
                pending.append((pi_versions[peer], wanted))
    answers = iter(_semver_satisfies_batch(fixture.release, pending)) if pending else iter(())
    cache = {pair: next(answers) for pair in pending}
    excluded = peer_excluded(manifests, pi_versions, lambda version, wanted: cache[(version, wanted)])
    return {"pi_versions": pi_versions, "excluded": excluded}


def build_fixture(cache: Path, lock: dict | None = None, *, force: bool = False, log=print) -> Fixture:
    lock = lock or load_lock()
    root = Path(cache) / lock_hash(lock)
    fixture = Fixture(root=root, lock=lock)
    if (root / "READY").exists() and not force:
        log(f"fixture cache hit: {root}")
        return fixture
    if shutil.which("node") is None or shutil.which("npm") is None:
        raise RuntimeError("node and npm are required to build the Pi fixture")
    staging = Path(cache) / f"{root.name}.building-{os.getpid()}"
    if staging.exists():
        shutil.rmtree(staging)
    building = Fixture(root=staging, lock=lock)
    started = time.monotonic()
    pi_version = lock["pi"]["version"]
    agent = building.agent
    (agent / "install" / "releases").mkdir(parents=True)

    # 1. Managed Pi release (mirrors ~/.pi/agent/bin/pi: install/current-version -> releases/<v>).
    release = building.release
    release.mkdir(parents=True)
    (release / "package.json").write_text(json.dumps({"name": "pi-managed", "private": True}, indent=2))
    result = _run(
        ["npm", "install", *NPM_EXTRA_FLAGS, "--prefix", str(release), f"{lock['pi']['package']}@{pi_version}"],
        cwd=release, log=log, timeout=900, env=_npm_env(),
    )
    if result.returncode != 0:
        raise RuntimeError(f"installing Pi {pi_version} failed:\n{(result.stdout + result.stderr)[-3000:]}")
    (agent / "install" / "current-version").write_text(f"{pi_version}\n")

    # 2. Packages, the way Pi's package manager installs them.
    install_root = agent / "npm"
    install_root.mkdir(parents=True)
    (install_root / "package.json").write_text(json.dumps({"name": "pi-extensions", "private": True}, indent=2))
    (install_root / ".gitignore").write_text("*\n!.gitignore\n")
    specs = package_specs(lock)
    npm_args = ["install", *specs, "--prefix", str(install_root), "--legacy-peer-deps", *NPM_EXTRA_FLAGS]
    result = _run(["npm", *npm_args], cwd=install_root, log=log, timeout=1800, env=_npm_env())
    install_mode = "batch"
    if result.returncode != 0:
        log("batch install failed; retrying one package at a time (Pi's installNpm)\n" + (result.stdout + result.stderr)[-1500:])
        install_mode = "sequential"
        failures = []
        for spec in specs:
            single = _run(
                ["npm", "install", spec, "--prefix", str(install_root), "--legacy-peer-deps", *NPM_EXTRA_FLAGS],
                cwd=install_root, log=log, timeout=900, env=_npm_env(),
            )
            if single.returncode != 0:
                failures.append(f"{spec}: {(single.stdout + single.stderr)[-500:]}")
        if failures:
            raise RuntimeError("package installs failed:\n" + "\n".join(failures))

    # 3. Sentinel + settings.
    (agent / "pi-eval").mkdir()
    shutil.copyfile(SENTINEL_SOURCE, agent / SENTINEL_SETTING)
    (agent / "settings.json").write_text(json.dumps(settings_document(lock), indent=2) + "\n")

    # 4. Record what was actually installed.
    installed = {}
    for entry in lock["packages"]:
        manifest = install_root / "node_modules" / entry["name"] / "package.json"
        installed[entry["name"]] = json.loads(manifest.read_text())["version"] if manifest.exists() else None
    missing = [name for name, version in installed.items() if version is None]
    if missing:
        raise RuntimeError(f"packages missing after install: {missing}")
    drift = {name: (entry["version"], installed[name]) for entry in lock["packages"] for name in [entry["name"]] if installed[name] != entry["version"]}
    if drift:
        raise RuntimeError(f"installed versions differ from the lock: {drift}")
    peers = compute_peer_exclusions(building)
    building.meta_path.write_text(
        json.dumps(
            {
                "layout": LAYOUT_VERSION,
                "lock_hash": root.name,
                "pi_version": pi_version,
                "installed": installed,
                "install_mode": install_mode,
                "npm_args": ["npm", "install", "<specs...>", "--prefix", "<agent>/npm", "--legacy-peer-deps", *NPM_EXTRA_FLAGS],
                "peer_excluded": peers["excluded"],
                "pi_package_versions": peers["pi_versions"],
                "built_s": round(time.monotonic() - started, 1),
            },
            indent=2,
        )
    )
    (staging / "READY").write_text("ok\n")
    if root.exists():
        shutil.rmtree(root)
    staging.rename(root)
    log(f"fixture built: {root} in {time.monotonic() - started:.0f}s")
    return Fixture(root=root, lock=lock)


def ensure_probe(fixture: Fixture, *, log=print, force: bool = False) -> dict:
    """Run Pi 1.4.0's own resource loader once to learn which packages load and what they register.

    Runs on a throwaway copy of the home so the cached template stays pristine.
    """
    if fixture.probe_path.exists() and not force:
        return json.loads(fixture.probe_path.read_text())
    scratch = fixture.root / f"probe-home-{os.getpid()}"
    if scratch.exists():
        shutil.rmtree(scratch)
    copy_tree(fixture.home, scratch)
    workspace = scratch / "workspace"
    workspace.mkdir()
    env = {key: value for key, value in os.environ.items() if not key.startswith(("PI_", "OCTET_"))}
    env.update({"HOME": str(scratch), "PI_OFFLINE": "1", "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0", "LANG": "C.UTF-8"})
    agent = scratch / ".pi" / "agent"
    release = agent / "install" / "releases" / fixture.lock["pi"]["version"]
    try:
        result = _run(
            ["node", str(PROBE_SCRIPT), str(release), str(agent), str(workspace)],
            cwd=workspace, log=log, timeout=300, env=env,
        )
        marker = [line for line in result.stdout.splitlines() if line.startswith("PI-PROBE-JSON ")]
        if result.returncode != 0 or not marker:
            raise RuntimeError(f"Pi probe failed (exit {result.returncode}):\n{(result.stdout + result.stderr)[-3000:]}")
        probe = json.loads(marker[-1][len("PI-PROBE-JSON "):])
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    fixture.probe_path.write_text(json.dumps(probe, indent=2))
    return probe


def write_provider_config(home: Path, port: int) -> None:
    """Point both Pi (models.json) and octet (custom credential) at the mock provider."""
    agent = home / ".pi" / "agent"
    agent.mkdir(parents=True, exist_ok=True)
    (agent / "models.json").write_text(
        json.dumps(
            {
                "providers": {
                    PROVIDER: {
                        "baseUrl": f"http://127.0.0.1:{port}/v1",
                        "api": "openai-completions",
                        "apiKey": "mock-key",
                        "models": [{"id": MODEL, "name": MODEL, "contextWindow": 128000, "maxTokens": 4096}],
                    }
                }
            },
            indent=2,
        )
    )
    credentials = home / ".octet" / "credentials"
    credentials.mkdir(parents=True, exist_ok=True)
    path = credentials / "custom.json"
    path.write_text(
        json.dumps(
            {
                "base_url": f"http://127.0.0.1:{port}/v1/",
                "api_key": "",
                "api_name": MODEL,
                "headers": [],
                "auto_discover": True,
            }
        )
    )
    path.chmod(0o600)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cache", type=Path, default=default_cache(), help="fixture cache directory (default: $PI_EVAL_CACHE or ~/.cache/octet-pi-eval)")
    parser.add_argument("--force", action="store_true", help="rebuild even if the cache is current")
    parser.add_argument("--probe", action="store_true", help="also run the Pi probe (executes the downloaded Pi 1.4.0 and its packages)")
    parser.add_argument("--print-hash", action="store_true", help="print the lock hash and exit")
    args = parser.parse_args(argv)
    lock = load_lock()
    if args.print_hash:
        print(lock_hash(lock))
        return 0
    fixture = build_fixture(args.cache, lock, force=args.force)
    print(fixture.root)
    if args.probe:
        probe = ensure_probe(fixture, force=args.force)
        print(f"probe: {len(probe['extensions'])} extensions loaded, {len(probe['errors'])} errors")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
