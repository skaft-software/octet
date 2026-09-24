#!/usr/bin/env python3
"""Refresh octet's checked-in OpenRouter image-model catalog.

This is an explicit maintainer operation. Normal builds and runtime never run
this script and never contact the network: `crates/octet-ai/src/images.rs`
embeds the checked-in `models/openrouter-image-models.json` snapshot.

The pinned source is a saved OpenRouter `/models` response supplied with
`--source`; this script performs no fetching of its own. Only routes whose
`architecture.output_modalities` contain `image` become catalog entries, input
modalities are limited to `text`/`image` (defaulting to `text`), and the four
per-token prices are converted to integer microdollars per million tokens in a
single deterministic pass. Outputs are byte-for-byte reproducible for one
source file, so `--check` can gate a refresh.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from decimal import Decimal, InvalidOperation, ROUND_HALF_UP
from pathlib import Path

MODELS_URL = "https://openrouter.ai/api/v1/models"
MAX_SNAPSHOT_BYTES = 16 * 1024 * 1024
DEFAULT_OUTPUT = Path("crates/octet-ai/models/openrouter-image-models.json")
DEFAULT_SOURCE_OUTPUT = Path("crates/octet-ai/models/openrouter-image-source.json")
IMAGE_API = "openrouter-images"
IMAGE_PROVIDER = "openrouter"
IMAGE_BASE_URL = "https://openrouter.ai/api/v1/"
IMAGE_OUTPUT_FILTER = "image"
ALLOWED_MODALITIES = ("text", "image")


def reject_json_constant(value: str) -> None:
    raise ValueError(f"non-standard JSON constant: {value}")


def microdollars(value: object | None) -> int:
    """Dollars per token -> microdollars per million tokens.

    Missing rates and OpenRouter's negative dynamic-routing placeholder are
    unknown, not free; only an explicit zero quotes a free rate.
    """
    if value is None or value == "":
        raise _Unpriced
    if isinstance(value, bool):
        raise ValueError(f"invalid OpenRouter price: {value!r}")
    try:
        amount = Decimal(str(value))
    except InvalidOperation as error:
        raise ValueError(f"invalid OpenRouter price: {value!r}") from error
    if not amount.is_finite():
        raise ValueError(f"invalid OpenRouter price: {value!r}")
    if amount < 0:
        raise _Unpriced
    return int(
        (amount * Decimal(1_000_000_000_000)).to_integral_value(rounding=ROUND_HALF_UP)
    )


class _Unpriced(Exception):
    """Marker for a dynamic-routing price placeholder."""


def model_cost(pricing: object) -> dict[str, int] | None:
    if not isinstance(pricing, dict):
        pricing = {}
    try:
        return {
            "input": microdollars(pricing.get("prompt")),
            "output": microdollars(pricing.get("completion")),
            "cache_read": microdollars(pricing.get("input_cache_read")),
            "cache_write": microdollars(pricing.get("input_cache_write")),
        }
    except _Unpriced:
        # Every billed bucket needs a rate: a missing or dynamic price makes
        # the whole cost unknown rather than undercounting a cost ceiling.
        return None


def unique_modalities(values: object) -> list[str]:
    if not isinstance(values, list):
        return []
    seen: list[str] = []
    for value in values:
        if value in ALLOWED_MODALITIES and value not in seen:
            seen.append(value)
    return seen


def image_catalog(payload: object) -> dict[str, object]:
    """Deterministic image-model catalog from one OpenRouter models response."""
    data = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(data, list) or not data:
        raise SystemExit("OpenRouter models response contained no model list")

    models: list[dict[str, object]] = []
    for record in data:
        if not isinstance(record, dict):
            continue
        architecture = record.get("architecture")
        output = unique_modalities(
            architecture.get("output_modalities") if isinstance(architecture, dict) else None
        )
        if IMAGE_OUTPUT_FILTER not in output:
            continue
        model_id = record.get("id")
        name = record.get("name")
        if not isinstance(model_id, str) or not model_id.strip():
            continue
        if not isinstance(name, str) or not name.strip():
            continue
        input_modalities = unique_modalities(
            architecture.get("input_modalities") if isinstance(architecture, dict) else None
        )
        if not input_modalities:
            input_modalities = ["text"]
        pricing = record.get("pricing") if isinstance(record.get("pricing"), dict) else {}
        models.append(
            {
                "id": model_id.strip(),
                "name": name.strip(),
                "api": IMAGE_API,
                "provider": IMAGE_PROVIDER,
                "base_url": IMAGE_BASE_URL,
                "input": input_modalities,
                "output": output,
                "cost": model_cost(pricing),
            }
        )

    models.sort(key=lambda model: model["id"])
    if not models:
        raise SystemExit("OpenRouter response contained no image models")
    return {"version": 1, "models": models}


def source_record(raw: bytes) -> dict[str, object]:
    return {
        "url": MODELS_URL,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "output_modalities_filter": IMAGE_OUTPUT_FILTER,
        "provider": IMAGE_PROVIDER,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--source",
        type=Path,
        required=True,
        help="saved OpenRouter /models response; this script never fetches",
    )
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--source-output", type=Path, default=DEFAULT_SOURCE_OUTPUT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="compare deterministic outputs without writing",
    )
    args = parser.parse_args()

    with args.source.open("rb") as source:
        raw = source.read(MAX_SNAPSHOT_BYTES + 1)
    if len(raw) > MAX_SNAPSHOT_BYTES:
        raise SystemExit("OpenRouter source exceeds the size limit")
    payload = json.loads(raw, parse_constant=reject_json_constant)
    outputs = {
        args.output: image_catalog(payload),
        args.source_output: source_record(raw),
    }
    # Validate and serialize everything before any destination is touched.
    texts = {
        path: json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
        for path, result in outputs.items()
    }
    for path, text in texts.items():
        if args.check:
            if not path.exists() or path.read_text() != text:
                raise SystemExit(f"stale OpenRouter image catalog: {path}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        record_count = (
            len(outputs[path]["models"]) if isinstance(outputs[path], dict) and "models" in outputs[path] else 1
        )
        print(f"{'checked' if args.check else 'wrote'} {record_count} records: {path}")


if __name__ == "__main__":
    main()
