#!/usr/bin/env python3
"""Refresh octet's checked-in models.dev pricing, display-name and capability snapshots.

This is an explicit maintainer operation. Normal builds never run this script
or contact the network.
"""

from __future__ import annotations

import argparse
import json
import hashlib
from decimal import Decimal, ROUND_HALF_UP
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

API_URL = "https://models.dev/api.json"
MAX_SNAPSHOT_BYTES = 16 * 1024 * 1024
DEFAULT_OUTPUT = Path("crates/octet-ai/models/models-dev-pricing.json")
DEFAULT_NAMES_OUTPUT = Path("crates/octet-ai/models/models-dev-names.json")
DEFAULT_CAPABILITIES_OUTPUT = Path("crates/octet-ai/models/models-dev-capabilities.json")
DEFAULT_SOURCE_OUTPUT = Path("crates/octet-ai/models/models-dev-source.json")

# Direct DeepSeek now publishes peak/off-peak rates. A flat catalog quote is
# not an authoritative schedule or safe upper bound for hard cost ceilings.
UNVERIFIED_PRICING_PROVIDERS = {"deepseek"}
CAPABILITY_FIELDS = (
    "name", "limit", "modalities", "tool_call", "structured_output",
    "reasoning", "reasoning_options", "interleaved",
)


# Provider catalogs can retain aliases that their own APIs reject. Keep these
# exclusions beside the refresh boundary so stale upstream data cannot make a
# dead route look supported or priced.
UNSUPPORTED_MODEL_IDS = {
    "openai": {"gpt-5.6"},
}

# Display-name aliases come from model-owner catalogs, not every downstream
# gateway. Including aggregators duplicates thousands of leaf IDs and removes
# useful unique aliases without adding a more authoritative name.
NAME_SOURCES = {
    "alibaba",
    "anthropic",
    "cohere",
    "deepreinforce",
    "deepseek",
    "google",
    "meituan",
    "meta",
    "microsoft",
    "minimax",
    "mistral",
    "moonshotai",
    "nvidia",
    "openai",
    "perplexity",
    "poolside",
    "sakana",
    "sarvam",
    "stepfun",
    "tencent",
    "thinkingmachines",
    "xai",
    "xiaomi",
    "zhipuai",
}

# octet's endpoint ids do not all use models.dev's provider ids. Keep this small
# route-identity mapping here; model names and rates come entirely from the
# downloaded catalog.
PROVIDER_SOURCES = {
    "anthropic": "anthropic",
    "cerebras": "cerebras",
    "deepseek": "deepseek",
    "fireworks": "fireworks-ai",
    "groq": "groq",
    "huggingface": "huggingface",
    "minimax": "minimax",
    "moonshotai": "moonshotai",
    "nvidia": "nvidia",
    "openai": "openai",
    "openrouter": "openrouter",
    "opencode": "opencode",
    "together": "togetherai",
    "xai": "xai",
    "xiaomi": "xiaomi",
}


def microdollars(value: object | None) -> int:
    if value is None:
        return 0
    if isinstance(value, bool):
        raise ValueError(f"invalid models.dev price: {value!r}")
    amount = Decimal(str(value))
    if not amount.is_finite() or amount < 0:
        raise ValueError(f"invalid models.dev price: {value!r}")
    return int((amount * Decimal(1_000_000)).to_integral_value(rounding=ROUND_HALF_UP))


def supported_model(provider_id: str, model_id: str) -> bool:
    return model_id not in UNSUPPORTED_MODEL_IDS.get(provider_id, set())


def names_snapshot(catalog: dict[str, object]) -> dict[str, str]:
    output: dict[str, str] = {}
    for provider_id in sorted(NAME_SOURCES):
        provider = catalog.get(provider_id)
        if not isinstance(provider, dict):
            continue
        models = provider.get("models")
        if not isinstance(models, dict):
            continue
        for model_id, model in sorted(models.items()):
            if (
                not isinstance(model_id, str)
                or not isinstance(model, dict)
                or not supported_model(provider_id, model_id)
            ):
                continue
            name = model.get("name")
            if not isinstance(name, str) or not name.strip():
                continue
            output[f"{provider_id}/{model_id}".lower()] = name.strip()
    return output


def snapshot(catalog: dict[str, object]) -> dict[str, dict[str, int | None]]:
    output: dict[str, dict[str, int | None]] = {}
    for octet_provider, source_provider in sorted(PROVIDER_SOURCES.items()):
        if octet_provider in UNVERIFIED_PRICING_PROVIDERS:
            continue
        provider = catalog.get(source_provider)
        if not isinstance(provider, dict):
            continue
        models = provider.get("models")
        if not isinstance(models, dict):
            continue
        for model_id, model in sorted(models.items()):
            if (
                not isinstance(model_id, str)
                or not isinstance(model, dict)
                or not supported_model(source_provider, model_id)
            ):
                continue
            cost = model.get("cost")
            if not isinstance(cost, dict) or "input" not in cost or "output" not in cost:
                continue
            if cost["input"] is None or cost["output"] is None:
                continue
            output[f"{octet_provider}/{model_id}".lower()] = {
                "cache_read": microdollars(cost.get("cache_read")),
                "cache_write_5m": microdollars(cost.get("cache_write")),
                "input": microdollars(cost["input"]),
                "output": microdollars(cost["output"]),
                "reasoning": (
                    None
                    if cost.get("reasoning") is None
                    else microdollars(cost["reasoning"])
                ),
            }
    return output


def capabilities_snapshot(catalog: dict[str, object]) -> dict[str, object]:
    """Keep source assertions intact, including unknowns; runtime fails closed.

    Keys are exact provider/model routes, never unique-leaf aliases. This is a
    metadata supplement, not an availability or protocol inventory.
    """
    output = {}
    for octet_provider, source_provider in sorted(PROVIDER_SOURCES.items()):
        provider = catalog.get(source_provider)
        if not isinstance(provider, dict) or not isinstance(provider.get("models"), dict):
            continue
        for model_id, model in sorted(provider["models"].items()):
            if not isinstance(model, dict) or not supported_model(source_provider, model_id):
                continue
            output[f"{octet_provider}/{model_id}"] = {
                key: model[key] for key in CAPABILITY_FIELDS if key in model
            }
    return output


class NoRedirects(HTTPRedirectHandler):
    """The fixed public HTTPS origin must serve the snapshot itself."""

    def redirect_request(self, request, fp, code, message, headers, new_url):
        raise HTTPError(request.full_url, code, "models.dev redirects are forbidden", headers, fp)


def download_source() -> bytes:
    # No environment proxies, cookies, netrc, credentials or alternate origins.
    # Rejecting every redirect is the zero-hop HTTPS-only redirect policy.
    opener = build_opener(ProxyHandler({}), NoRedirects())
    request = Request(API_URL, headers={
        "Accept": "application/json", "User-Agent": "octet-model-refresh/1"})
    with opener.open(request, timeout=30) as response:
        length = response.headers.get("Content-Length")
        if length is not None and (not length.isdecimal() or int(length) > MAX_SNAPSHOT_BYTES):
            raise ValueError("models.dev snapshot exceeds size limit or has invalid Content-Length")
        raw = response.read(MAX_SNAPSHOT_BYTES + 1)
    if len(raw) > MAX_SNAPSHOT_BYTES:
        raise ValueError("models.dev snapshot exceeds size limit")
    return raw


def reject_json_constant(value: str) -> None:
    raise ValueError(f"invalid JSON constant: {value}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path,
                        help="local api.json fixture; downloads models.dev when omitted")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--names-output", type=Path, default=DEFAULT_NAMES_OUTPUT)
    parser.add_argument("--capabilities-output", type=Path, default=DEFAULT_CAPABILITIES_OUTPUT)
    parser.add_argument("--source-output", type=Path, default=DEFAULT_SOURCE_OUTPUT)
    parser.add_argument("--check", action="store_true",
                        help="compare deterministic outputs without writing")
    args = parser.parse_args()
    if args.source:
        with args.source.open("rb") as source:
            raw = source.read(MAX_SNAPSHOT_BYTES + 1)
    else:
        raw = download_source()
    if len(raw) > MAX_SNAPSHOT_BYTES:
        raise SystemExit("models.dev snapshot exceeds size limit")
    catalog = json.loads(raw, parse_constant=reject_json_constant)
    if not isinstance(catalog, dict):
        raise SystemExit("models.dev api.json must contain an object")
    outputs = {
        args.output: snapshot(catalog),
        args.names_output: names_snapshot(catalog),
        args.capabilities_output: capabilities_snapshot(catalog),
        args.source_output: {
            "url": API_URL,
            "sha256": hashlib.sha256(raw).hexdigest(),
            "unverified_pricing_providers": sorted(UNVERIFIED_PRICING_PROVIDERS),
        },
    }
    for path, result in outputs.items():
        if not result:
            raise SystemExit(f"models.dev api.json contained no records for {path}")
    # Validate/serialize all outputs before any destination is touched.
    texts = {path: json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
             for path, result in outputs.items()}
    for path, text in texts.items():
        if args.check:
            if not path.exists() or path.read_text() != text:
                raise SystemExit(f"stale models.dev snapshot: {path}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        print(f"{'checked' if args.check else 'wrote'} {len(outputs[path])} records: {path}")
    if UNVERIFIED_PRICING_PROVIDERS:
        print("WARNING: omitted unverified schedule pricing: " +
              ", ".join(sorted(UNVERIFIED_PRICING_PROVIDERS)))


if __name__ == "__main__":
    main()
