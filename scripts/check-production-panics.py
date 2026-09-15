#!/usr/bin/env python3
"""Audit production Cargo JSON diagnostics for unjustified unwrap/expect lints.

This checker intentionally consumes Cargo's JSONL output instead of searching Rust
source text. It fails closed when an artifact is missing, malformed, incomplete,
or outside the production/test target classification understood by the audit.
"""

from __future__ import annotations

import argparse
import json
import os
from dataclasses import dataclass
from pathlib import Path
import sys
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple


LINT_CODES = ("clippy::expect_used", "clippy::unwrap_used")
PRODUCTION_TARGET_KINDS = frozenset(
    {
        "bin",
        "cdylib",
        "custom-build",
        "dylib",
        "lib",
        "proc-macro",
        "staticlib",
    }
)
TEST_ONLY_TARGET_KINDS = frozenset({"bench", "example", "test"})
SUPPORTED_REASONS = frozenset(
    {
        "build-finished",
        "build-script-executed",
        "compiler-artifact",
        "compiler-executable",
        "compiler-message",
    }
)
EVIDENCE_KINDS = frozenset(
    {
        "build-script-contract",
        "documentation",
        "regression-test",
        "source-invariant",
    }
)


class AuditError(Exception):
    """Raised for a failed audit or an invalid audit input."""


@dataclass(frozen=True)
class Finding:
    """A normalized production lint diagnostic."""

    fingerprint: str
    lint: str
    target: str
    target_kind: str
    file: str
    line: int
    column: int
    message: str

    def describe(self) -> str:
        return (
            f"{self.fingerprint} ({self.message})"
            f" at {self.file}:{self.line}:{self.column}"
        )


@dataclass(frozen=True)
class AuditResult:
    """The findings that matched a reviewed baseline."""

    findings: Tuple[Finding, ...]
    diagnostic_artifacts: Tuple[str, ...]


def make_fingerprint(
    lint: str,
    target_kind: str,
    target: str,
    file: str,
    line: int,
    column: int,
) -> str:
    """Return the stable, human-readable identity used by the baseline."""

    return f"{lint}|{target_kind}|{target}|{file}:{line}:{column}"


def _no_duplicate_pairs(pairs: List[Tuple[str, Any]]) -> Dict[str, Any]:
    result: Dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON object key: {key}")
        result[key] = value
    return result


def _reject_json_constant(value: str) -> None:
    raise ValueError(f"non-standard JSON constant: {value}")


def _read_jsonl(path: Path) -> List[Tuple[int, Dict[str, Any]]]:
    if not path.is_file():
        raise AuditError(f"diagnostic artifact does not exist: {path}")

    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise AuditError(f"cannot read diagnostic artifact {path}: {exc}") from exc

    records: List[Tuple[int, Dict[str, Any]]] = []
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        if not raw_line.strip():
            continue
        try:
            value = json.loads(
                raw_line,
                object_pairs_hook=_no_duplicate_pairs,
                parse_constant=_reject_json_constant,
            )
        except (TypeError, ValueError, json.JSONDecodeError) as exc:
            raise AuditError(
                f"invalid Cargo JSON at {path}:{line_number}: {exc}"
            ) from exc
        if not isinstance(value, dict):
            raise AuditError(
                f"Cargo JSON record at {path}:{line_number} is not an object"
            )
        records.append((line_number, value))

    if not records:
        raise AuditError(f"diagnostic artifact is empty: {path}")
    return records


def _read_json(path: Path) -> Dict[str, Any]:
    if not path.is_file():
        raise AuditError(f"baseline does not exist: {path}")
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise AuditError(f"cannot read baseline {path}: {exc}") from exc
    if not text.strip():
        raise AuditError(f"baseline is empty: {path}")
    try:
        value = json.loads(
            text,
            object_pairs_hook=_no_duplicate_pairs,
            parse_constant=_reject_json_constant,
        )
    except (TypeError, ValueError, json.JSONDecodeError) as exc:
        raise AuditError(f"invalid baseline JSON in {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise AuditError("baseline root must be a JSON object")
    return value


def _required_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise AuditError(f"{label} must be a non-empty string")
    return value


def _positive_integer(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        raise AuditError(f"{label} must be a positive integer")
    return value


def _target(target: Any, context: str) -> Tuple[str, Tuple[str, ...]]:
    if not isinstance(target, dict):
        raise AuditError(f"{context}.target must be an object")
    name = _required_string(target.get("name"), f"{context}.target.name")
    kinds = target.get("kind")
    if not isinstance(kinds, list) or not kinds:
        raise AuditError(f"{context}.target.kind must be a non-empty array")
    normalized: List[str] = []
    for index, kind in enumerate(kinds):
        normalized_kind = _required_string(
            kind, f"{context}.target.kind[{index}]"
        )
        if normalized_kind in normalized:
            raise AuditError(f"{context}.target.kind contains a duplicate")
        normalized.append(normalized_kind)
    crate_types = target.get("crate_types")
    if not isinstance(crate_types, list) or not crate_types or any(
        not isinstance(crate_type, str) or not crate_type.strip()
        for crate_type in crate_types
    ):
        raise AuditError(f"{context}.target.crate_types must be a non-empty array")
    _required_string(target.get("src_path"), f"{context}.target.src_path")
    _required_string(target.get("edition"), f"{context}.target.edition")
    for field in ("doc", "test", "doctest"):
        if not isinstance(target.get(field), bool):
            raise AuditError(f"{context}.target.{field} must be a boolean")
    return name, tuple(sorted(normalized))


def _target_classification(kinds: Sequence[str]) -> str:
    kind_set = set(kinds)
    has_production_kind = bool(kind_set & PRODUCTION_TARGET_KINDS)
    has_test_kind = bool(kind_set & TEST_ONLY_TARGET_KINDS)
    if has_production_kind and has_test_kind:
        return "unknown"
    if has_test_kind:
        return "test-only"
    if has_production_kind:
        return "production"
    return "unknown"


def _validate_artifact(record: Mapping[str, Any], context: str) -> str:
    _required_string(record.get("package_id"), f"{context}.package_id")
    _required_string(record.get("manifest_path"), f"{context}.manifest_path")
    _target_name, kinds = _target(record.get("target"), context)
    if not isinstance(record.get("profile"), dict):
        raise AuditError(f"{context}.profile must be an object")
    filenames = record.get("filenames")
    if not isinstance(filenames, list) or not filenames or any(
        not isinstance(filename, str) or not filename.strip()
        for filename in filenames
    ):
        raise AuditError(f"{context}.filenames must be a non-empty array of strings")
    if "executable" not in record or (
        record.get("executable") is not None
        and not isinstance(record.get("executable"), str)
    ):
        raise AuditError(f"{context}.executable must be null or a string")
    if not isinstance(record.get("features"), list) or any(
        not isinstance(feature, str) for feature in record["features"]
    ):
        raise AuditError(f"{context}.features must be an array of strings")
    if not isinstance(record.get("fresh"), bool):
        raise AuditError(f"{context}.fresh must be a boolean")
    classification = _target_classification(kinds)
    if classification == "unknown":
        raise AuditError(
            f"{context}.target.kind cannot be classified as production or test-only"
        )
    return classification


def _validate_message_record(
    record: Mapping[str, Any], context: str
) -> Tuple[str, Tuple[str, ...], str, Dict[str, Any]]:
    _required_string(record.get("package_id"), f"{context}.package_id")
    target_name, kinds = _target(record.get("target"), context)
    message = record.get("message")
    if not isinstance(message, dict):
        raise AuditError(f"{context}.message must be an object")
    _required_string(message.get("level"), f"{context}.message.level")
    _required_string(message.get("message"), f"{context}.message.message")
    if "code" not in message:
        raise AuditError(f"{context}.message.code is missing")
    code = message.get("code")
    if code is not None:
        if not isinstance(code, dict):
            raise AuditError(f"{context}.message.code must be null or an object")
        _required_string(code.get("code"), f"{context}.message.code.code")
    spans = message.get("spans")
    if not isinstance(spans, list):
        raise AuditError(f"{context}.message.spans must be an array")
    if not isinstance(message.get("children"), list):
        raise AuditError(f"{context}.message.children must be an array")
    if not isinstance(message.get("rendered"), str):
        raise AuditError(f"{context}.message.rendered must be a string")
    for span_index, span in enumerate(spans):
        if not isinstance(span, dict):
            raise AuditError(f"{context}.message.spans[{span_index}] must be an object")
        if not isinstance(span.get("is_primary"), bool):
            raise AuditError(
                f"{context}.message.spans[{span_index}].is_primary must be a boolean"
            )
    classification = _target_classification(kinds)
    if classification == "unknown":
        raise AuditError(
            f"{context}.target.kind cannot be classified as production or test-only"
        )
    return target_name, kinds, classification, message


def _repository_file(file_name: str, repository_root: Path, context: str) -> str:
    if "\\" in file_name:
        raise AuditError(f"{context}.file_name must use normalized separators")
    root = repository_root.resolve()
    candidate = Path(file_name)
    if not candidate.is_absolute():
        candidate = root / candidate
    try:
        resolved = candidate.resolve()
        relative = resolved.relative_to(root)
    except ValueError as exc:
        raise AuditError(
            f"{context}.file_name is outside the repository root: {file_name}"
        ) from exc
    if not resolved.is_file():
        raise AuditError(f"{context}.file_name is not a repository file: {file_name}")
    normalized = relative.as_posix()
    if not normalized or normalized == ".":
        raise AuditError(f"{context}.file_name is not a file path")
    return normalized


def _finding_from_message(
    record: Mapping[str, Any], repository_root: Path, context: str
) -> Optional[Finding]:
    target_name, kinds, classification, message = _validate_message_record(
        record, context
    )
    code = message["code"]
    lint = code.get("code") if isinstance(code, dict) else None
    if lint not in LINT_CODES:
        return None
    if classification == "test-only":
        return None
    if classification != "production":
        raise AuditError(f"{context} has an unclassified lint target")
    if message.get("level") != "warning":
        raise AuditError(
            f"{context} lint {lint} must be a warning-level diagnostic"
        )

    spans = message["spans"]
    primary_spans = [
        span
        for span in spans
        if isinstance(span, dict) and span.get("is_primary") is True
    ]
    if len(primary_spans) != 1:
        raise AuditError(
            f"{context} lint {lint} must have exactly one primary source span"
        )
    span = primary_spans[0]
    file_name = _required_string(span.get("file_name"), f"{context}.span.file_name")
    file = _repository_file(file_name, repository_root, f"{context}.span")
    line = _positive_integer(span.get("line_start"), f"{context}.span.line_start")
    column = _positive_integer(
        span.get("column_start"), f"{context}.span.column_start"
    )
    target_kind = ",".join(kinds)
    message_text = _required_string(message.get("message"), f"{context}.message")
    fingerprint = make_fingerprint(
        lint, target_kind, target_name, file, line, column
    )
    return Finding(
        fingerprint=fingerprint,
        lint=lint,
        target=target_name,
        target_kind=target_kind,
        file=file,
        line=line,
        column=column,
        message=message_text,
    )


def _parse_artifact(path: Path, repository_root: Path) -> List[Finding]:
    records = _read_jsonl(path)
    findings: List[Finding] = []
    production_artifacts = 0
    build_finished_records = 0

    for line_number, record in records:
        reason = record.get("reason")
        if not isinstance(reason, str) or not reason:
            raise AuditError(f"{path}:{line_number} has no Cargo message reason")
        if reason not in SUPPORTED_REASONS:
            raise AuditError(f"{path}:{line_number} has unsupported reason: {reason}")

        context = f"{path}:{line_number}"
        if reason == "compiler-artifact":
            if _validate_artifact(record, context) == "production":
                production_artifacts += 1
        elif reason == "compiler-message":
            finding = _finding_from_message(record, repository_root, context)
            if finding is not None:
                findings.append(finding)
        elif reason == "build-finished":
            success = record.get("success")
            if not isinstance(success, bool):
                raise AuditError(f"{context}.success must be a boolean")
            build_finished_records += 1
            if not success:
                raise AuditError(f"Cargo reported an unsuccessful build in {path}")
        elif reason in {"build-script-executed", "compiler-executable"}:
            _required_string(record.get("package_id"), f"{context}.package_id")
            if reason == "build-script-executed":
                for field in ("linked_libs", "linked_paths", "cfgs", "env"):
                    value = record.get(field)
                    if not isinstance(value, list):
                        raise AuditError(f"{context}.{field} must be an array")
                _required_string(record.get("out_dir"), f"{context}.out_dir")
            else:
                _target(record.get("target"), context)
                _required_string(record.get("executable"), f"{context}.executable")

    if production_artifacts == 0:
        raise AuditError(
            f"diagnostic artifact has no classified production compiler artifact: {path}"
        )
    if build_finished_records != 1:
        raise AuditError(
            f"diagnostic artifact must contain exactly one successful build-finished record: {path}"
        )
    return findings


def _normalized_baseline_file(value: Any, label: str) -> str:
    file = _required_string(value, label)
    if "\\" in file or file.startswith("/"):
        raise AuditError(f"{label} must be a repository-relative POSIX path")
    parts = file.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        raise AuditError(f"{label} must be a normalized repository-relative path")
    return file


def _validate_scope(scope: Any) -> None:
    if not isinstance(scope, dict):
        raise AuditError("baseline.scope must be an object")
    expected = {
        "rust_msrv": "1.86",
        "diagnostic_format": "cargo-jsonl",
        "feature_selection": "all-features",
        "lint_level": "warning",
        "target_selection": ["--lib", "--bins"],
        "target_exclusions": ["test", "example", "bench"],
        "lint_codes": list(LINT_CODES),
        "production_target_kinds": sorted(PRODUCTION_TARGET_KINDS),
    }
    if set(scope) != set(expected):
        raise AuditError("baseline.scope has unexpected or missing fields")
    for key, expected_value in expected.items():
        if scope.get(key) != expected_value:
            raise AuditError(
                f"baseline.scope.{key} does not match the production audit contract"
            )


def _validate_evidence(value: Any, index: int) -> None:
    if not isinstance(value, list) or not value:
        raise AuditError(f"baseline.findings[{index}].evidence must be non-empty")
    for evidence_index, evidence in enumerate(value):
        label = f"baseline.findings[{index}].evidence[{evidence_index}]"
        if not isinstance(evidence, dict):
            raise AuditError(f"{label} must be an object")
        if set(evidence) != {"kind", "reference"}:
            raise AuditError(f"{label} has unexpected or missing fields")
        kind = evidence.get("kind")
        if kind not in EVIDENCE_KINDS:
            raise AuditError(f"{label}.kind is not a recognized evidence kind")
        _required_string(evidence.get("reference"), f"{label}.reference")


def _validate_baseline_finding(value: Any, index: int) -> Finding:
    label = f"baseline.findings[{index}]"
    if not isinstance(value, dict):
        raise AuditError(f"{label} must be an object")
    expected_fields = {
        "fingerprint",
        "lint",
        "target",
        "target_kind",
        "file",
        "line",
        "column",
        "message",
        "reviewed",
        "justification",
        "evidence",
    }
    if set(value) != expected_fields:
        raise AuditError(f"{label} has unexpected or missing fields")
    if value.get("reviewed") is not True:
        raise AuditError(f"{label}.reviewed must be true")
    lint = value.get("lint")
    if lint not in LINT_CODES:
        raise AuditError(f"{label}.lint is not an audited Clippy code")
    target = _required_string(value.get("target"), f"{label}.target")
    target_kind = _required_string(value.get("target_kind"), f"{label}.target_kind")
    target_kind_parts = target_kind.split(",")
    if target_kind_parts != sorted(set(target_kind_parts)) or any(
        kind not in PRODUCTION_TARGET_KINDS for kind in target_kind_parts
    ):
        raise AuditError(f"{label}.target_kind is not a normalized production kind")
    file = _normalized_baseline_file(value.get("file"), f"{label}.file")
    line = _positive_integer(value.get("line"), f"{label}.line")
    column = _positive_integer(value.get("column"), f"{label}.column")
    message = _required_string(value.get("message"), f"{label}.message")
    justification = _required_string(
        value.get("justification"), f"{label}.justification"
    )
    if len(justification.strip()) < 20:
        raise AuditError(f"{label}.justification is too short to be review evidence")
    _validate_evidence(value.get("evidence"), index)

    expected_fingerprint = make_fingerprint(
        lint, target_kind, target, file, line, column
    )
    if value.get("fingerprint") != expected_fingerprint:
        raise AuditError(f"{label}.fingerprint does not match its identity fields")
    return Finding(
        fingerprint=expected_fingerprint,
        lint=lint,
        target=target,
        target_kind=target_kind,
        file=file,
        line=line,
        column=column,
        message=message,
    )


def _load_baseline(path: Path) -> Tuple[str, Dict[str, Finding]]:
    value = _read_json(path)
    if value.get("schema_version") != 1:
        raise AuditError("baseline.schema_version must be 1")
    if value.get("tool") != "cargo clippy":
        raise AuditError("baseline.tool must be cargo clippy")
    status = value.get("status")
    if status not in {"collected", "uncollected"}:
        raise AuditError("baseline.status must be collected or uncollected")
    expected_fields = {
        "schema_version",
        "tool",
        "status",
        "scope",
        "findings",
    }
    if status == "collected":
        expected_fields.add("collection")
    if set(value) != expected_fields:
        raise AuditError("baseline has unexpected or missing fields")
    _validate_scope(value.get("scope"))

    raw_findings = value.get("findings")
    if not isinstance(raw_findings, list):
        raise AuditError("baseline.findings must be an array")
    findings: Dict[str, Finding] = {}
    for index, raw_finding in enumerate(raw_findings):
        finding = _validate_baseline_finding(raw_finding, index)
        if finding.fingerprint in findings:
            raise AuditError(
                f"baseline contains duplicate fingerprint: {finding.fingerprint}"
            )
        findings[finding.fingerprint] = finding

    if status == "uncollected":
        if findings:
            raise AuditError("an uncollected baseline cannot contain findings")
    else:
        collection = value.get("collection")
        if not isinstance(collection, dict):
            raise AuditError("a collected baseline requires collection metadata")
        if set(collection) != {"toolchain", "artifacts"}:
            raise AuditError("baseline.collection has unexpected or missing fields")
        toolchain = _required_string(collection.get("toolchain"), "collection.toolchain")
        if toolchain != "1.86.0":
            raise AuditError("collection.toolchain must be Rust 1.86.0")
        artifacts = collection.get("artifacts")
        if not isinstance(artifacts, list) or not artifacts:
            raise AuditError("collection.artifacts must be a non-empty list")
        for index, artifact in enumerate(artifacts):
            _normalized_baseline_file(
                artifact, f"collection.artifacts[{index}]"
            )

    return status, findings


def audit(
    diagnostic_paths: Sequence[os.PathLike[str] | str],
    baseline_path: os.PathLike[str] | str,
    repository_root: os.PathLike[str] | str,
) -> AuditResult:
    """Validate complete diagnostics against a reviewed production baseline."""

    if not diagnostic_paths:
        raise AuditError("at least one Cargo JSONL diagnostic artifact is required")
    root = Path(repository_root)
    all_findings: Dict[str, Finding] = {}
    artifact_names: List[str] = []
    for raw_path in diagnostic_paths:
        path = Path(raw_path)
        findings = _parse_artifact(path, root)
        artifact_names.append(str(path))
        for finding in findings:
            if finding.fingerprint in all_findings:
                raise AuditError(
                    "duplicate production diagnostic fingerprint across artifacts: "
                    f"{finding.fingerprint}"
                )
            all_findings[finding.fingerprint] = finding

    status, baseline = _load_baseline(Path(baseline_path))
    if status != "collected":
        raise AuditError(
            "baseline is uncollected; real Rust 1.86 Cargo JSONL evidence must be "
            "reviewed before this gate can pass"
        )

    current_keys = set(all_findings)
    baseline_keys = set(baseline)
    missing = sorted(current_keys - baseline_keys)
    stale = sorted(baseline_keys - current_keys)
    changed = sorted(
        key
        for key in current_keys & baseline_keys
        if all_findings[key] != baseline[key]
    )
    if missing or stale or changed:
        details: List[str] = []
        if missing:
            details.append(
                "new production findings: "
                + "; ".join(all_findings[key].describe() for key in missing)
            )
        if stale:
            details.append("stale baseline fingerprints: " + "; ".join(stale))
        if changed:
            details.append(
                "reviewed baseline metadata mismatch: " + "; ".join(changed)
            )
        raise AuditError("production panic baseline mismatch: " + " | ".join(details))

    return AuditResult(
        findings=tuple(all_findings[key] for key in sorted(all_findings)),
        diagnostic_artifacts=tuple(artifact_names),
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Check production Cargo Clippy JSONL diagnostics against a reviewed baseline."
    )
    parser.add_argument(
        "--diagnostics",
        nargs="+",
        required=True,
        metavar="JSONL",
        help="one or more complete cargo --message-format=json artifacts",
    )
    parser.add_argument(
        "--baseline",
        default=str(
            Path(__file__).resolve().parents[1]
            / "scripts"
            / "production-panic-baseline.json"
        ),
        help="reviewed baseline JSON path",
    )
    parser.add_argument(
        "--repository-root",
        default=str(Path(__file__).resolve().parents[1]),
        help="repository root used to normalize diagnostic source paths",
    )
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = _parser().parse_args(argv)
    try:
        result = audit(args.diagnostics, args.baseline, args.repository_root)
    except AuditError as exc:
        print(f"production panic audit failed: {exc}", file=sys.stderr)
        return 1
    print(
        "production panic audit passed: "
        f"{len(result.findings)} reviewed findings matched across "
        f"{len(result.diagnostic_artifacts)} diagnostic artifact(s)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
