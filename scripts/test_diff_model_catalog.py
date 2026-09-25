#!/usr/bin/env python3
"""Offline regressions for the HEAD/worktree model catalog diff."""

from __future__ import annotations

import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).with_name("diff-model-catalog.py")
SPEC = importlib.util.spec_from_file_location("diff_model_catalog", SCRIPT)
diff_model_catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(diff_model_catalog)

PATH = "crates/octet-ai/models/catalog.json"


def catalog(models: list[dict]) -> dict:
    return {"endpoints": [{"id": "openai", "base_url": "https://api.openai.com/v1/"}],
            "models": models}


def model(model_id: str, reasoning: dict | None = None, context: int = 128000) -> dict:
    return {
        "id": model_id,
        "endpoint": "openai",
        "protocol": "open_ai_chat",
        "api_name": model_id,
        "capabilities": {"tools": True, "reasoning": reasoning},
        "limits": {"context_window": context, "max_output_tokens": 16384},
    }


class EffectiveReasoningLevelsTests(unittest.TestCase):
    def test_explicit_option_values_are_resolved_and_deduplicated(self) -> None:
        levels = diff_model_catalog.effective_reasoning_levels(
            {
                "control": "effort",
                "options": {"values": ["low", "medium", "high", "xhigh", "max"], "default": "low"},
            }
        )
        self.assertEqual(levels, ["low", "medium", "high", "xhigh", "max"])
        aliased = diff_model_catalog.effective_reasoning_levels(
            {"control": "effort", "options": {"values": ["min", "minimal", "high--bogus"]}}
        )
        self.assertEqual(aliased, ["minimal"])

    def test_token_budget_options_are_the_effective_levels(self) -> None:
        levels = diff_model_catalog.effective_reasoning_levels(
            {
                "control": "token_budget",
                "effort_budgets": {"minimal": 1024, "high": 6144},
                "min_effort": "minimal",
                "max_effort": "high",
                "options": {"values": ["none", "minimal", "low", "medium", "high"], "default": "medium"},
            }
        )
        self.assertEqual(levels, ["off", "minimal", "low", "medium", "high"])

    def test_effort_range_is_clamped_and_scaled(self) -> None:
        self.assertEqual(
            diff_model_catalog.effective_reasoning_levels(
                {"control": "effort", "min_effort": "low", "max_effort": "max"}
            ),
            ["off", "low", "medium", "high", "xhigh", "max"],
        )
        self.assertEqual(
            diff_model_catalog.effective_reasoning_levels({"control": "always_on"}),
            ["on"],
        )
        self.assertEqual(
            diff_model_catalog.effective_reasoning_levels({"control": "toggle"}),
            ["off", "on"],
        )
        self.assertIsNone(diff_model_catalog.effective_reasoning_levels(None))


class CatalogDiffTests(unittest.TestCase):
    def test_added_removed_and_changed_models_are_reported(self) -> None:
        base = catalog([model("a"), model("b"), model("c")])
        worktree = catalog([model("a"), model("b", context=200000), model("d")])
        diff = diff_model_catalog.catalog_diff(base, worktree)
        self.assertEqual(diff["models"]["removed"], ["openai/c"])
        self.assertEqual(diff["models"]["added"], ["openai/d"])
        changed = {(entry["endpoint"], entry["model"]) for entry in diff["models"]["changed"]}
        self.assertEqual(changed, {("openai", "b")})
        self.assertFalse(diff_model_catalog.diff_is_empty(diff))

    def test_reasoning_level_changes_are_called_out(self) -> None:
        base = catalog([
            model("a", {"control": "effort", "options": {"values": ["low", "high"]}}),
            model("b", {"control": "effort", "min_effort": "minimal", "max_effort": "high"}),
        ])
        worktree = catalog([
            model("a", {"control": "effort", "options": {"values": ["low", "medium", "high"]}}),
            model("b", {"control": "effort", "min_effort": "low", "max_effort": "high"}),
        ])
        diff = diff_model_catalog.catalog_diff(base, worktree)
        reported = {entry["model"]: (entry["from"], entry["to"])
                    for entry in diff["reasoning_levels_changed"]}
        self.assertEqual(reported["a"], (["low", "high"], ["low", "medium", "high"]))
        self.assertEqual(
            reported["b"], (["off", "minimal", "low", "medium", "high"], ["off", "low", "medium", "high"])
        )

    def test_identical_catalogs_produce_an_empty_diff(self) -> None:
        document = catalog([model("a", {"control": "effort", "min_effort": "low"})])
        diff = diff_model_catalog.catalog_diff(json.loads(json.dumps(document)), document)
        self.assertTrue(diff_model_catalog.diff_is_empty(diff))


class CommandTests(unittest.TestCase):
    def _repository(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / PATH).parent.mkdir(parents=True)
        (root / PATH).write_text(json.dumps(catalog([model("a")])), encoding="utf-8")
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(["git", "add", PATH], cwd=root, check=True)
        subprocess.run(
            ["git", "-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "base"],
            cwd=root,
            check=True,
        )
        return root

    def _run(self, *argv: str, cwd: Path | None = None) -> tuple[int, str, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = diff_model_catalog.main(list(argv))
        return code, stdout.getvalue(), stderr.getvalue()

    def test_unchanged_worktree_checks_clean(self) -> None:
        root = self._repository()
        code, out, _ = self._run("--root", str(root), "--check")
        self.assertEqual(code, 0)
        self.assertIn("no catalog differences", out)

    def test_changed_worktree_fails_check_and_prints_levels(self) -> None:
        root = self._repository()
        (root / PATH).write_text(
            json.dumps(
                catalog([
                    model("a", {"control": "effort", "options": {"values": ["low", "medium", "high"]}}),
                    model("b"),
                ])
            ),
            encoding="utf-8",
        )
        code, out, _ = self._run("--root", str(root), "--check")
        self.assertEqual(code, 1)
        self.assertIn("openai/b", out)
        self.assertIn("effective reasoning levels: openai/a", out)

    def test_json_output_is_deterministic(self) -> None:
        root = self._repository()
        (root / PATH).write_text(json.dumps(catalog([model("b")])), encoding="utf-8")
        code, out, _ = self._run("--root", str(root), "--json")
        self.assertEqual(code, 0)
        parsed = json.loads(out)
        self.assertEqual(parsed["models"]["added"], ["openai/b"])
        self.assertEqual(parsed["models"]["removed"], ["openai/a"])

    def test_missing_catalog_reports_usage_error(self) -> None:
        root = self._repository()
        (root / PATH).unlink()
        code, _, err = self._run("--root", str(root))
        self.assertEqual(code, 2)
        self.assertIn("missing from the worktree", err)


if __name__ == "__main__":
    unittest.main()
