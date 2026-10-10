"""Disposable octet homes, workspaces and extension fixtures for the suite."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path


#: The three real Pi packages from ORDERS/matrix exercised by the compat smoke.
#: Entry paths are relative to `<matrix>/<name>/pkg/package/`.
PI_COMPAT_PACKAGES: tuple[tuple[str, str], ...] = (
    ("pi-ding", "extensions/ding.ts"),
    ("pi-powerline-footer", "index.ts"),
    ("pi-hermes-memory", "src/index.ts"),
)


class FixtureUnavailable(RuntimeError):
    """A prerequisite for a check is missing on this host; report as SKIP."""


@dataclass
class Environment:
    root: Path
    name: str
    provider_port: int
    model: str = "custom/mock-1"

    @property
    def home(self) -> Path:
        return self.root / "home"

    @property
    def workspace(self) -> Path:
        return self.root / "workspace"

    @property
    def sessions(self) -> Path:
        return self.root / "sessions"

    @property
    def extensions(self) -> Path:
        return self.root / "extensions"

    @property
    def model_label(self) -> str:
        return self.model.split("/", 1)[1]

    def child_env(self, repo_root: Path, *, extra: dict[str, str] | None = None) -> dict[str, str]:
        env = {key: value for key, value in os.environ.items() if not key.startswith("OCTET_")}
        env.update(
            {
                "HOME": str(self.home),
                "TERM": "xterm-256color",
                "COLORTERM": "truecolor",
                "LANG": "C.UTF-8",
                "LC_ALL": "C.UTF-8",
                # Match U8's light terminal so the light theme variants are the
                # ones under assertion, independent of the CI host's palette.
                "OCTET_COLOR_SCHEME": "light",
                # The hello-world fixture imports the in-repo Python SDK.
                "PYTHONPATH": str(repo_root / "sdk" / "python"),
            }
        )
        if extra:
            env.update(extra)
        return env


def build_environment(root: Path, name: str, provider_port: int, *, model: str = "custom/mock-1") -> Environment:
    env = Environment(root=root / name, name=name, provider_port=provider_port, model=model)
    (env.home / ".octet" / "credentials").mkdir(parents=True)
    env.workspace.mkdir(parents=True)
    env.sessions.mkdir(parents=True)
    env.extensions.mkdir(parents=True)
    credentials = env.home / ".octet" / "credentials" / "custom.json"
    credentials.write_text(
        json.dumps(
            {
                "base_url": f"http://127.0.0.1:{provider_port}/v1/",
                "api_key": "",
                "api_name": model.split("/", 1)[1],
                "headers": [],
                "auto_discover": False,
            }
        )
    )
    credentials.chmod(0o600)
    return env


def octet_args(
    env: Environment,
    *,
    name: str | None,
    mouse: str = "app",
    theme: str | None = None,
    extension_dirs: list[Path] | None = None,
    enable_extensions: list[str] | None = None,
    agent: bool = True,
    columns: int = 120,
    rows: int = 40,
    extra: list[str] | None = None,
) -> list[str]:
    args = [
        "--offline",
        "--no-context-files",
        "--color",
        "always",
        "--workspace",
        str(env.workspace),
        "--session-dir",
        str(env.sessions),
        "--model",
        env.model,
        "--mouse",
        mouse,
    ]
    if name is not None:
        args += ["--name", name]
    if agent:
        args += ["--no-tools"]
    if theme is not None:
        args += ["--theme", theme]
    for directory in extension_dirs or []:
        args += ["--extension-dir", str(directory)]
    if enable_extensions:
        args += ["--enable-extension", ",".join(enable_extensions)]
    if extra:
        args += extra
    return args


def install_hello_world(env: Environment, repo_root: Path) -> str:
    """Copy the in-repo legacy hello-world example into the scratch home.

    The host clears the extension child's environment; the scratch copy points
    its manifest `[entrypoint] env` at the in-repo Python SDK so the fixture is
    self-contained (the checked-in example expects an installed SDK).
    """
    source = repo_root / "examples" / "extensions" / "hello-world"
    target = env.extensions / "hello-world"
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(source, target)
    manifest = (target / "extension.toml").read_text()
    sdk = repo_root / "sdk" / "python"
    manifest = manifest.replace(
        '[entrypoint]\ncommand = "extension.py"\n',
        '[entrypoint]\ncommand = "extension.py"\nenv = { PYTHONPATH = %s }\n' % json.dumps(str(sdk)),
        1,
    )
    (target / "extension.toml").write_text(manifest)
    return "hello-world"


def configure_pi_compat(env: Environment, repo_root: Path, matrix_root: Path) -> dict:
    """Generate a reviewed octet-pi-compat extension dir for the three packages.

    Returns the parsed bridge configuration (registrations included). Raises
    FixtureUnavailable when the adapter, Node, or the matrix packages are not
    present on this host.
    """
    adapter = repo_root / "extensions" / "octet-pi-compat"
    configure = adapter / "configure.mjs"
    if not configure.exists():
        raise FixtureUnavailable(f"adapter configure script missing: {configure}")
    if shutil.which("node") is None:
        raise FixtureUnavailable("node is required to configure Pi extensions")
    if not (adapter / "node_modules" / "jiti").exists():
        raise FixtureUnavailable(
            f"adapter dependencies are not installed in {adapter}; run npm ci there once"
        )
    entrypoints = []
    missing = []
    for name, relative in PI_COMPAT_PACKAGES:
        entry = matrix_root / name / "pkg" / "package" / relative
        if entry.exists():
            entrypoints.append(entry)
        else:
            missing.append(str(entry))
    if missing:
        raise FixtureUnavailable(
            "matrix packages are not installed on this host; missing: " + ", ".join(missing)
        )
    output = env.extensions / "octet-pi-compat"
    if output.exists():
        shutil.rmtree(output)
    result = subprocess.run(
        ["node", str(configure), "--reviewed", "--output", str(output), *[str(e) for e in entrypoints]],
        cwd=adapter,
        capture_output=True,
        text=True,
        timeout=180,
    )
    if result.returncode != 0:
        raise FixtureUnavailable(
            "octet-pi-compat configure failed:\n"
            + (result.stdout + result.stderr).strip()[-4000:]
        )
    bridge = json.loads((output / "bridge.json").read_text())
    registrations = bridge.get("registrations", bridge)
    return {
        "output": output,
        "commands": [entry["name"] for entry in registrations.get("commands", [])],
        "tools": [entry["name"] for entry in registrations.get("tools", [])],
    }
