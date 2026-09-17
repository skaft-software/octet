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

# A reviewed source correction, like the exclusions above: pi keeps Baseten's
# rename-based GLM-5.2 endpoints text-only even though models.dev reports image
# input (`scripts/generate-models.ts` `supportsImageInput = !isGlm52 && ...`).
# Only the pinned input modalities are corrected; every other leaf is kept.
TEXT_ONLY_MODEL_IDS = {
    ("baseten", "zai-org/GLM-5.2"),
    ("baseten", "zai-org/GLM-5.2-Fast"),
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
    "baseten": "baseten",
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
    # The Alibaba Token Plan and Zhipu coding-plan sources feed octet's renamed
    # token-plan/coding routes (`scripts/generate-models.ts` 2388, 1265). The
    # Individual subscription is the same international source, narrowed to the
    # documented personal allowlist below.
    "qwen-token-plan": "alibaba-token-plan",
    "qwen-token-plan-cn": "alibaba-token-plan-cn",
    "qwen-token-plan-individual": "alibaba-token-plan",
    "together": "togetherai",
    "xai": "xai",
    "xiaomi": "xiaomi",
    "zai-coding-cn": "zhipuai-coding-plan",
}

# Upstream's generated catalogs keep only routes the provider actually serves:
# a tool-capable model, not deprecated, not retired, and (for the Individual
# subscription) inside the documented personal allowlist. These are membership
# rules for the pinned snapshot, not wire assertions; a route upstream does not
# emit must not become a priced octet route.
MODEL_REQUIRES_TOOL_CALL = {
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "zai-coding-cn",
}
MODEL_SKIPS_DEPRECATED = {"baseten"}
# Retired Alibaba Token Plan alias excluded for every variant
# (`scripts/generate-models.ts` `QWEN_TOKEN_PLAN_EXCLUDED_MODEL_IDS`).
EXCLUDED_MODEL_IDS = {
    "alibaba-token-plan": {"qwen3.8-max-preview"},
    "alibaba-token-plan-cn": {"qwen3.8-max-preview"},
}
# QwenCloud Token Plan Individual text-model allowlist, verified upstream
# 2026-09-03 (`QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS`).
QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS = {
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "glm-5.2",
    "qwen3.6-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
}
MODEL_ALLOWLISTS = {"qwen-token-plan-individual": QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS}
# The coding-plan entry can publish no rate; pi quotes the equivalent `zai`
# catalog there. When neither publishes input and output rates the route stays
# unpriced rather than fabricated.
PRICING_FALLBACK_SOURCES = {"zai-coding-cn": "zai"}


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


def catalog_model_included(
    octet_provider: str,
    source_provider: str,
    model_id: str,
    model: object,
) -> bool:
    """Whether upstream's generated catalog emits this pinned route.

    `model` stays a source assertion, never a wire profile: only membership is
    decided here so a retired or non-tool route cannot look available.
    """
    if not isinstance(model, dict):
        return False
    if model_id in EXCLUDED_MODEL_IDS.get(source_provider, set()):
        return False
    allowlist = MODEL_ALLOWLISTS.get(octet_provider)
    if allowlist is not None and model_id not in allowlist:
        return False
    if octet_provider in MODEL_REQUIRES_TOOL_CALL and model.get("tool_call") is not True:
        return False
    if octet_provider in MODEL_SKIPS_DEPRECATED and model.get("status") == "deprecated":
        return False
    return True


def pinned_cost(
    catalog: dict[str, object],
    octet_provider: str,
    source_provider: str,
    model_id: str,
    model: dict[str, object],
) -> dict[str, object] | None:
    """Exact published rates for one pinned route, with pi's reference fallback.

    Partial or absent rates stay absent so hard cost ceilings remain unknown
    rather than priced from a neighbouring catalog.
    """
    cost = model.get("cost")
    if (
        isinstance(cost, dict)
        and cost.get("input") is not None
        and cost.get("output") is not None
    ):
        return cost
    fallback_id = PRICING_FALLBACK_SOURCES.get(octet_provider)
    if fallback_id is None or fallback_id == source_provider:
        return None
    fallback = catalog.get(fallback_id)
    fallback_models = fallback.get("models") if isinstance(fallback, dict) else None
    entry = fallback_models.get(model_id) if isinstance(fallback_models, dict) else None
    reference = entry.get("cost") if isinstance(entry, dict) else None
    if (
        isinstance(reference, dict)
        and reference.get("input") is not None
        and reference.get("output") is not None
    ):
        return reference
    return None


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
                or not supported_model(source_provider, model_id)
                or not catalog_model_included(octet_provider, source_provider, model_id, model)
            ):
                continue
            cost = pinned_cost(catalog, octet_provider, source_provider, model_id, model)
            if cost is None:
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
            if (
                not isinstance(model_id, str)
                or not supported_model(source_provider, model_id)
                or not catalog_model_included(octet_provider, source_provider, model_id, model)
            ):
                continue
            record = {key: model[key] for key in CAPABILITY_FIELDS if key in model}
            modalities = record.get("modalities")
            if (
                (octet_provider, model_id) in TEXT_ONLY_MODEL_IDS
                and isinstance(modalities, dict)
                and isinstance(modalities.get("input"), list)
            ):
                record["modalities"] = {
                    **modalities,
                    "input": [item for item in modalities["input"] if item != "image"],
                }
            output[f"{octet_provider}/{model_id}"] = record
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
