#!/usr/bin/env python3
"""Diff the checked-in model catalog between a git ref and the worktree.

Reports added/removed/changed endpoints and models, and -- for every model that
changed -- its *effective reasoning levels*: the exact choice set the product
picker and strict core validation actually expose, mirroring
`ReasoningCapability::choices()` in `crates/octet-ai/src/types.rs`.

Offline and deterministic: reads `git show <ref>:<path>` plus the worktree file,
never the network. Exits 1 under `--check` when anything differs.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

DEFAULT_PATH = "crates/octet-ai/models/catalog.json"
DEFAULT_REF = "HEAD"

# Semantic effort ordering; mirrors `ReasoningEffort` declaration order.
EFFORT_ORDER = ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"]

# Portable aliases accepted by `ReasoningConfig::from_provider_value`.
ALIASES = {
    "none": "off",
    "off": "off",
    "disabled": "off",
    "false": "off",
    "default": "on",
    "on": "on",
    "enabled": "on",
    "true": "on",
    "minimal": "minimal",
    "min": "minimal",
    "low": "low",
    "medium": "medium",
    "med": "medium",
    "high": "high",
    "xhigh": "xhigh",
    "max": "max",
    "ultra": "ultra",
}


class UsageError(Exception):
    """A deterministic command or input failure; reported without a traceback."""


def canonical_selector(value: str) -> str | None:
    """Map one provider spelling to its portable selector, or `None` if unknown."""
    return ALIASES.get(value.strip().lower())


def effective_reasoning_levels(reasoning: Any) -> list[str] | None:
    """Mirror `ReasoningCapability::choices()` for one serialized capability.

    `None` means the model advertises no reasoning contract at all.
    """
    if not isinstance(reasoning, dict):
        return None
    control = reasoning.get("control")
    if control == "always_on":
        return ["on"]
    values = None
    options = reasoning.get("options")
    if isinstance(options, dict) and isinstance(options.get("values"), list):
        values = options["values"]
    else:
        mode = reasoning.get("openai_chat_mode")
        if isinstance(mode, dict) and isinstance(mode.get("values"), list):
            values = mode["values"]
    if values is not None:
        resolved: list[str] = []
        for value in values:
            selector = canonical_selector(str(value))
            if selector is not None and selector not in resolved:
                resolved.append(selector)
        return resolved
    if control == "toggle":
        return ["off", "on"]
    low = canonical_selector(str(reasoning.get("min_effort", "minimal"))) or "minimal"
    high = canonical_selector(str(reasoning.get("max_effort", "high"))) or "high"
    try:
        low_index = EFFORT_ORDER.index(low)
    except ValueError:
        low_index = 0
    try:
        high_index = EFFORT_ORDER.index(high)
    except ValueError:
        high_index = len(EFFORT_ORDER) - 1
    if low_index > high_index:
        low_index, high_index = high_index, low_index
    levels = ["off"]
    levels.extend(
        effort
        for index, effort in enumerate(EFFORT_ORDER)
        if low_index <= index <= high_index
    )
    return levels


def _index(endpoints: Any) -> dict[str, dict[str, Any]]:
    if not isinstance(endpoints, list):
        return {}
    result: dict[str, dict[str, Any]] = {}
    for endpoint in endpoints:
        if isinstance(endpoint, dict) and isinstance(endpoint.get("id"), str):
            result[endpoint["id"]] = endpoint
    return result


def _model_index(models: Any) -> dict[tuple[str, str], dict[str, Any]]:
    if not isinstance(models, list):
        return {}
    result: dict[tuple[str, str], dict[str, Any]] = {}
    for model in models:
        if not isinstance(model, dict):
            continue
        model_id = model.get("id")
        endpoint = model.get("endpoint")
        if isinstance(model_id, str) and isinstance(endpoint, str):
            result[(endpoint, model_id)] = model
    return result


def _model_summary(model: dict[str, Any]) -> dict[str, Any]:
    capabilities = model.get("capabilities") or {}
    limits = model.get("limits") or {}
    return {
        "protocol": model.get("protocol"),
        "api_name": model.get("api_name"),
        "context_window": limits.get("context_window"),
        "max_output_tokens": limits.get("max_output_tokens"),
        "tools": capabilities.get("tools"),
        "reasoning_levels": effective_reasoning_levels(capabilities.get("reasoning")),
    }


def catalog_diff(base: Any | None, worktree: Any | None) -> dict[str, Any]:
    """Return a bounded, deterministic diff between two catalog documents."""
    base = base if isinstance(base, dict) else {}
    worktree = worktree if isinstance(worktree, dict) else {}

    base_endpoints = _index(base.get("endpoints"))
    worktree_endpoints = _index(worktree.get("endpoints"))
    base_models = _model_index(base.get("models"))
    worktree_models = _model_index(worktree.get("models"))

    endpoint_added = sorted(set(worktree_endpoints) - set(base_endpoints))
    endpoint_removed = sorted(set(base_endpoints) - set(worktree_endpoints))
    endpoint_changed = sorted(
        name
        for name in set(base_endpoints) & set(worktree_endpoints)
        if base_endpoints[name].get("base_url") != worktree_endpoints[name].get("base_url")
        or base_endpoints[name].get("auth") != worktree_endpoints[name].get("auth")
    )

    model_added = sorted(set(worktree_models) - set(base_models))
    model_removed = sorted(set(base_models) - set(worktree_models))

    changed: list[dict[str, Any]] = []
    for key in sorted(set(base_models) & set(worktree_models)):
        before = _model_summary(base_models[key])
        after = _model_summary(worktree_models[key])
        fields = {
            field: {"from": before[field], "to": after[field]}
            for field in before
            if before[field] != after[field]
        }
        if fields:
            changed.append({"endpoint": key[0], "model": key[1], "fields": fields})

    reasoning_changed = [
        {"endpoint": entry["endpoint"], "model": entry["model"],
         "from": entry["fields"]["reasoning_levels"]["from"],
         "to": entry["fields"]["reasoning_levels"]["to"]}
        for entry in changed
        if "reasoning_levels" in entry["fields"]
    ]

    return {
        "endpoints": {
            "added": endpoint_added,
            "removed": endpoint_removed,
            "changed": endpoint_changed,
        },
        "models": {
            "added": [f"{endpoint}/{model}" for endpoint, model in model_added],
            "removed": [f"{endpoint}/{model}" for endpoint, model in model_removed],
            "changed": changed,
        },
        "reasoning_levels_changed": reasoning_changed,
    }


def diff_is_empty(diff: dict[str, Any]) -> bool:
    endpoints = diff["endpoints"]
    models = diff["models"]
    return not (
        endpoints["added"]
        or endpoints["removed"]
        or endpoints["changed"]
        or models["added"]
        or models["removed"]
        or models["changed"]
    )


def _git_show(ref: str, path: str, root: str) -> str:
    process = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if process.returncode != 0:
        message = process.stderr.strip() or "unknown git failure"
        raise UsageError(f"cannot read {path} at {ref}: {message}")
    return process.stdout


def _load(text: str, origin: str) -> Any:
    try:
        return json.loads(text)
    except json.JSONDecodeError as error:
        raise UsageError(f"{origin} is not valid JSON: {error}") from error


def render_human(diff: dict[str, Any], ref: str, path: str) -> str:
    lines = [f"# model catalog diff: {ref}:{path} -> worktree"]
    endpoints = diff["endpoints"]
    models = diff["models"]
    lines.append(f"endpoints added:   {', '.join(endpoints['added']) or '-'}")
    lines.append(f"endpoints removed: {', '.join(endpoints['removed']) or '-'}")
    lines.append(f"endpoints changed: {', '.join(endpoints['changed']) or '-'}")
    lines.append(f"models added:      {', '.join(models['added']) or '-'}")
    lines.append(f"models removed:    {', '.join(models['removed']) or '-'}")
    if not models["changed"]:
        lines.append("models changed:    -")
    else:
        lines.append(f"models changed:    {len(models['changed'])}")
        for entry in models["changed"]:
            lines.append(f"- {entry['endpoint']}/{entry['model']}")
            for field, values in entry["fields"].items():
                lines.append(
                    f"    {field}: {json.dumps(values['from'])} -> {json.dumps(values['to'])}"
                )
    for entry in diff["reasoning_levels_changed"]:
        lines.append(
            "effective reasoning levels: "
            f"{entry['endpoint']}/{entry['model']} "
            f"{json.dumps(entry['from'])} -> {json.dumps(entry['to'])}"
        )
    if diff_is_empty(diff):
        lines.append("no catalog differences")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", default=DEFAULT_REF, help="git ref to compare (default HEAD)")
    parser.add_argument("--path", default=DEFAULT_PATH, help="catalog path")
    parser.add_argument("--root", default=".", help="repository root (default .)")
    parser.add_argument("--json", action="store_true", help="emit JSON instead of text")
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 when the worktree differs from the ref",
    )
    options = parser.parse_args(argv)

    worktree_path = Path(options.root) / options.path
    try:
        base = _load(
            _git_show(options.ref, options.path, options.root),
            f"{options.ref}:{options.path}",
        )
        if not worktree_path.is_file():
            raise UsageError(f"{worktree_path} is missing from the worktree")
        worktree = _load(worktree_path.read_text(encoding="utf-8"), str(worktree_path))
    except UsageError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    diff = catalog_diff(base, worktree)
    if options.json:
        print(json.dumps(diff, indent=2, sort_keys=True))
    else:
        print(render_human(diff, options.ref, options.path))

    if options.check and not diff_is_empty(diff):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
