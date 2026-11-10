#!/usr/bin/env python3
"""Report empirical Pi-compat coverage from a real-CLI comparison run.

The compatibility ledger measures how much of an actual Pi setup works through
`octet-pi-compat`, not how much of Pi's API Octet re-implements natively. This
script is that measurement: it reads the receipts written by `prompts.py` and
reports, per behaviour, whether Octet matched the reference Pi CLI.

Coverage is the fraction of cases where Octet's semantic provider request
matched Pi's. A case that failed in both is still Octet not working, so it counts
against coverage; only a match counts for it.

Usage:
    python3 -B scripts/pi-eval/coverage.py /private/tmp/octet-prompt-comparison
"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    run = Path(argv[1])
    if not (run / "results.jsonl").exists():
        print(f"no results.jsonl in {run}", file=sys.stderr)
        return 2

    inputs = json.loads((run / "inputs.json").read_text())
    results = read_jsonl(run / "results.jsonl")
    comparisons = read_jsonl(run / "comparisons.jsonl")

    cases = inputs["cases"]
    references = sorted({row["reference"] for row in comparisons})
    legs: dict[tuple[str, str, int], dict] = {}
    for row in results:
        legs[(row["target"], row["case"], row["rep"])] = row

    matched: dict[str, set[tuple[str, int]]] = {ref: set() for ref in references}

    def leg_ok(target: str, case: str, rep: int) -> bool:
        row = legs.get((target, case, rep))
        return bool(row) and not row["errors"]

    for row in comparisons:
        # Coverage requires the behaviour to actually work on both sides: an
        # Octet leg with its own errors is a failure even if its (empty or
        # degraded) observations happen to equal Pi's.
        if (row["semantic_request_match"]
                and leg_ok("octet", row["case"], row["rep"])
                and leg_ok(row["reference"], row["case"], row["rep"])):
            matched[row["reference"]].add((row["case"], row["rep"]))

    # Per-case: did every repetition match at least one reference?
    print(f"run: {run}")
    print(f"binary: {inputs.get('octet')}  sha256: {inputs.get('octet_sha256', '')[:16]}")
    print(f"head: {inputs.get('head', '')[:12]}  dirty: {inputs.get('source_dirty')}")
    print()
    header = f"{'case':<34}" + "".join(f"{ref:<12}" for ref in references)
    print(header)
    print("-" * len(header))
    per_case_pass = 0
    for case in cases:
        cells = []
        ok_any = False
        for ref in references:
            reps = [rep for (c, rep) in matched[ref] if c == case]
            ok = len(reps) == inputs["reps"]
            ok_any = ok_any or ok
            cells.append("pass" if ok else "FAIL")
        per_case_pass += ok_any
        print(f"{case:<34}" + "".join(f"{cell:<12}" for cell in cells))
    print()

    for ref in references:
        passed = len(matched[ref])
        denominator = len(cases) * inputs["reps"]
        print(f"octet vs {ref}: {passed}/{denominator} repetitions matched"
              f"  ({100 * passed / denominator:.0f}% coverage)")
    print(f"cases working against every reference: {per_case_pass}/{len(cases)}")

    failures = [
        f"  {row['case']} [{row['reference']}] rep {row['rep']}: "
        + "; ".join(legs[("octet", row["case"], row["rep"])]["errors"])
        for row in comparisons
        if not row["semantic_request_match"]
        and legs.get(("octet", row["case"], row["rep"]), {}).get("errors")
    ]
    if failures:
        print("\noctet failures:")
        print("\n".join(failures))

    return 0 if per_case_pass == len(cases) else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
