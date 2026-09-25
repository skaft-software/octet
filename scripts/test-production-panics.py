#!/usr/bin/env python3
"""Deterministic tests for the production panic audit contract."""

from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
CHECKER_PATH = ROOT / "scripts" / "check-production-panics.py"
SOURCE_PATH = CHECKER_PATH

_spec = importlib.util.spec_from_file_location("check_production_panics", CHECKER_PATH)
if _spec is None or _spec.loader is None:
    raise RuntimeError(f"cannot import {CHECKER_PATH}")
checker = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = checker
_spec.loader.exec_module(checker)

SCOPE = {
    "rust_msrv": "1.86",
    "diagnostic_format": "cargo-jsonl",
    "feature_selection": "all-features",
    "lint_level": "warning",
    "target_selection": ["--lib", "--bins"],
    "target_exclusions": ["test", "example", "bench"],
    "lint_codes": ["clippy::expect_used", "clippy::unwrap_used"],
    "production_target_kinds": [
        "bin", "cdylib", "custom-build", "dylib", "lib", "proc-macro", "staticlib"
    ],
}


def cargo_target(name: str, kinds: tuple[str, ...] = ("lib",)) -> dict[str, object]:
    return {
        "kind": list(kinds), "crate_types": list(kinds), "name": name,
        "src_path": str(SOURCE_PATH), "edition": "2021", "doc": True,
        "test": True, "doctest": True,
    }


def compiler_artifact(
    name: str = "fixture-lib", kinds: tuple[str, ...] = ("lib",)
) -> dict[str, object]:
    return {
        "reason": "compiler-artifact",
        "package_id": "path+file:///fixture#fixture@0.1.0",
        "manifest_path": str(ROOT / "Cargo.toml"),
        "target": cargo_target(name, kinds),
        "profile": {"test": False}, "features": [],
        "filenames": [str(ROOT / "target" / "debug" / "libfixture.rlib")],
        "executable": None, "fresh": False,
    }


def compiler_message(
    lint: str | None, *, name: str = "fixture-lib",
    kinds: tuple[str, ...] = ("lib",), line: int = 10, column: int = 3,
    message_text: str = "fixture diagnostic", level: str = "warning",
    source: Path = SOURCE_PATH, primary_spans: int = 1,
) -> dict[str, object]:
    spans = []
    for index in range(primary_spans):
        spans.append({
            "file_name": str(source), "byte_start": index, "byte_end": index + 1,
            "line_start": line, "line_end": line, "column_start": column,
            "column_end": column + 1, "is_primary": True,
            "text": [{"text": "fixture", "highlight_start": 1, "highlight_end": 2}],
            "label": None, "suggested_replacement": None,
            "suggestion_applicability": None, "expansion": None,
        })
    code: object = None if lint is None else {"code": lint, "explanation": None}
    return {
        "reason": "compiler-message",
        "package_id": "path+file:///fixture#fixture@0.1.0",
        "target": cargo_target(name, kinds),
        "message": {
            "message": message_text, "code": code, "level": level,
            "spans": spans, "children": [], "rendered": message_text + "\n",
        },
    }


def build_finished(success: bool = True) -> dict[str, object]:
    return {"reason": "build-finished", "success": success}


def baseline_finding(
    lint: str = "clippy::unwrap_used", *, target: str = "fixture-lib",
    target_kind: str = "lib", line: int = 10, column: int = 3,
    message: str = "fixture diagnostic",
) -> dict[str, object]:
    file = SOURCE_PATH.relative_to(ROOT).as_posix()
    return {
        "fingerprint": checker.make_fingerprint(
            lint, target_kind, target, file, line, column
        ),
        "lint": lint, "target": target, "target_kind": target_kind,
        "file": file, "line": line, "column": column, "message": message,
        "reviewed": True,
        "justification": (
            "The reviewed invariant makes this diagnostic unreachable in production."
        ),
        "evidence": [{"kind": "source-invariant", "reference": f"{file}:{line}"}],
    }


def baseline(
    findings: list[dict[str, object]], *, status: str = "collected"
) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": 1, "tool": "cargo clippy", "status": status,
        "scope": copy.deepcopy(SCOPE), "findings": findings,
    }
    if status == "collected":
        value["collection"] = {"toolchain": "1.86.0", "artifacts": ["clippy.jsonl"]}
    return value


class ProductionPanicAuditTests(unittest.TestCase):
    def run_audit(
        self, records: list[dict[str, object]], baseline_value: dict[str, object], *,
        raw_artifact: str | None = None,
    ) -> checker.AuditResult:
        with tempfile.TemporaryDirectory() as directory:
            directory_path = Path(directory)
            artifact_path = directory_path / "clippy.jsonl"
            if raw_artifact is None:
                artifact_path.write_text(
                    "".join(json.dumps(record) + "\n" for record in records),
                    encoding="utf-8",
                )
            else:
                artifact_path.write_text(raw_artifact, encoding="utf-8")
            baseline_path = directory_path / "baseline.json"
            baseline_path.write_text(json.dumps(baseline_value), encoding="utf-8")
            return checker.audit([artifact_path], baseline_path, ROOT)

    def assert_audit_error(
        self, records: list[dict[str, object]], baseline_value: dict[str, object],
        expected: str, *, raw_artifact: str | None = None,
    ) -> None:
        with self.assertRaises(checker.AuditError) as context:
            self.run_audit(records, baseline_value, raw_artifact=raw_artifact)
        self.assertIn(expected, str(context.exception))

    @staticmethod
    def normal_records(
        *extra: dict[str, object], artifact_name: str = "fixture-lib",
        artifact_kinds: tuple[str, ...] = ("lib",),
    ) -> list[dict[str, object]]:
        return [compiler_artifact(artifact_name, artifact_kinds), *extra, build_finished()]

    def test_reviewed_production_finding_matches(self) -> None:
        result = self.run_audit(
            self.normal_records(compiler_message("clippy::unwrap_used")),
            baseline([baseline_finding()]),
        )
        self.assertEqual(len(result.findings), 1)
        self.assertEqual(result.findings[0].fingerprint, baseline_finding()["fingerprint"])

    def test_test_example_and_bench_targets_are_excluded(self) -> None:
        records = [
            compiler_artifact("fixture-lib", ("lib",)),
            compiler_artifact("fixture-test", ("test",)),
            compiler_artifact("fixture-example", ("example",)),
            compiler_artifact("fixture-bench", ("bench",)),
            compiler_message("clippy::unwrap_used", name="fixture-test", kinds=("test",)),
            compiler_message("clippy::expect_used", name="fixture-example", kinds=("example",)),
            compiler_message("clippy::unwrap_used", name="fixture-bench", kinds=("bench",)),
            build_finished(),
        ]
        result = self.run_audit(records, baseline([]))
        self.assertEqual(result.findings, ())

    def test_non_lint_production_messages_do_not_create_findings(self) -> None:
        result = self.run_audit(self.normal_records(compiler_message(None)), baseline([]))
        self.assertEqual(result.findings, ())

    def test_new_and_stale_findings_fail_closed(self) -> None:
        self.assert_audit_error(
            self.normal_records(compiler_message("clippy::unwrap_used")), baseline([]),
            "new production findings",
        )
        self.assert_audit_error(
            self.normal_records(), baseline([baseline_finding()]),
            "stale baseline fingerprints",
        )

    def test_changed_reviewed_metadata_requires_re_review(self) -> None:
        self.assert_audit_error(
            self.normal_records(compiler_message("clippy::unwrap_used")),
            baseline([baseline_finding(message="a changed compiler diagnostic")]),
            "reviewed baseline metadata mismatch",
        )

    def test_uncollected_baseline_never_passes(self) -> None:
        self.assert_audit_error(
            self.normal_records(), baseline([], status="uncollected"),
            "baseline is uncollected; real Rust 1.86 Cargo JSONL evidence",
        )

    def test_malformed_or_incomplete_jsonl_fails_closed(self) -> None:
        self.assert_audit_error([], baseline([]), "invalid Cargo JSON", raw_artifact="not-json\n")
        self.assert_audit_error(
            [compiler_artifact()], baseline([]),
            "exactly one successful build-finished record",
        )
        self.assert_audit_error(
            [compiler_artifact(), build_finished(False)], baseline([]), "unsuccessful build"
        )
        self.assert_audit_error(
            [], baseline([]), "duplicate JSON object key",
            raw_artifact='{"reason":"compiler-artifact","reason":"build-finished"}\n',
        )
        self.assert_audit_error(
            [], baseline([]), "non-standard JSON constant", raw_artifact='{"reason":NaN}\n'
        )

    def test_unknown_and_mixed_target_kinds_fail_closed(self) -> None:
        self.assert_audit_error(
            self.normal_records(artifact_kinds=("unknown",)), baseline([]),
            "cannot be classified as production or test-only",
        )
        self.assert_audit_error(
            self.normal_records(artifact_kinds=("lib", "test")), baseline([]),
            "cannot be classified as production or test-only",
        )

    def test_lint_span_and_level_contract_is_strict(self) -> None:
        self.assert_audit_error(
            self.normal_records(compiler_message("clippy::unwrap_used", primary_spans=0)),
            baseline([]), "exactly one primary source span",
        )
        self.assert_audit_error(
            self.normal_records(compiler_message("clippy::unwrap_used", primary_spans=2)),
            baseline([]), "exactly one primary source span",
        )
        self.assert_audit_error(
            self.normal_records(compiler_message("clippy::unwrap_used", level="error")),
            baseline([]), "must be a warning-level diagnostic",
        )
        self.assert_audit_error(
            self.normal_records(compiler_message("clippy::unwrap_used", source=Path("/tmp/outside.rs"))),
            baseline([]), "outside the repository root",
        )

    def test_review_evidence_and_fingerprint_are_required(self) -> None:
        finding = baseline_finding()
        not_reviewed = copy.deepcopy(finding)
        not_reviewed["reviewed"] = False
        self.assert_audit_error(self.normal_records(), baseline([not_reviewed]), "reviewed must be true")
        no_evidence = copy.deepcopy(finding)
        no_evidence["evidence"] = []
        self.assert_audit_error(self.normal_records(), baseline([no_evidence]), "evidence must be non-empty")
        bad_fingerprint = copy.deepcopy(finding)
        bad_fingerprint["fingerprint"] = "not-a-fingerprint"
        self.assert_audit_error(self.normal_records(), baseline([bad_fingerprint]), "fingerprint does not match")
        short_justification = copy.deepcopy(finding)
        short_justification["justification"] = "too short"
        self.assert_audit_error(self.normal_records(), baseline([short_justification]), "justification is too short")

    def test_duplicate_fingerprints_are_rejected(self) -> None:
        self.assert_audit_error(
            self.normal_records(
                compiler_message("clippy::unwrap_used"),
                compiler_message("clippy::unwrap_used"),
            ),
            baseline([baseline_finding()]), "duplicate production diagnostic fingerprint",
        )

    def test_supported_non_diagnostic_framing_records_are_validated(self) -> None:
        build_script = {
            "reason": "build-script-executed", "package_id": "path+file:///fixture#fixture@0.1.0",
            "linked_libs": [], "linked_paths": [], "cfgs": [], "env": [],
            "out_dir": str(ROOT / "target" / "debug" / "build"),
        }
        executable = {
            "reason": "compiler-executable", "package_id": "path+file:///fixture#fixture@0.1.0",
            "target": cargo_target("fixture-bin", ("bin",)),
            "executable": str(ROOT / "target" / "debug" / "fixture"),
        }
        result = self.run_audit(self.normal_records(build_script, executable), baseline([]))
        self.assertEqual(result.findings, ())

    def test_baseline_scope_and_collection_metadata_are_strict(self) -> None:
        invalid_scope = baseline([])
        invalid_scope["scope"]["unexpected"] = True  # type: ignore[index]
        self.assert_audit_error(self.normal_records(), invalid_scope, "baseline.scope has unexpected or missing fields")
        missing_collection = baseline([])
        del missing_collection["collection"]
        self.assert_audit_error(self.normal_records(), missing_collection, "baseline has unexpected or missing fields")
        invalid_toolchain = baseline([])
        invalid_toolchain["collection"]["toolchain"] = "1.87.0"  # type: ignore[index]
        self.assert_audit_error(self.normal_records(), invalid_toolchain, "collection.toolchain must be Rust 1.86.0")

    def test_repository_workflow_is_manual_immutable_and_pinned(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "production-panic-audit.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotIn("\n  push:", workflow)
        self.assertNotIn("\n  pull_request:", workflow)
        self.assertIn("source_sha", workflow)
        self.assertIn("GITHUB_SHA", workflow)
        self.assertIn("--all-features", workflow)
        self.assertIn("--lib --bins", workflow)
        self.assertIn("--message-format=json", workflow)
        self.assertIn("check-production-panics.py", workflow)
        self.assertIn("if: always()", workflow)
        uses = re.findall(r"^\s*- uses:\s*([^\s]+)", workflow, re.MULTILINE)
        self.assertTrue(uses)
        for action in uses:
            self.assertRegex(action, r"@[0-9a-f]{40}$")

    def test_committed_baseline_is_explicitly_uncollected(self) -> None:
        value = json.loads((ROOT / "scripts" / "production-panic-baseline.json").read_text(encoding="utf-8"))
        self.assertEqual(value["status"], "uncollected")
        self.assertEqual(value["findings"], [])
        self.assertEqual(value["scope"], SCOPE)
        self.assertNotIn("collection", value)


if __name__ == "__main__":
    unittest.main()
