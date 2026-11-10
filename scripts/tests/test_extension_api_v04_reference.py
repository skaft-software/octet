"""API 0.4's maintained reference must never be a 0.3 generator output."""

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts/generate-extension-api-v03.py"
spec = importlib.util.spec_from_file_location("extension_api_generator", GENERATOR)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class MaintainedReferenceTests(unittest.TestCase):
    def test_reference_is_not_a_generated_output(self):
        source = generator.SOURCE.read_bytes()
        outputs = generator.generated_files(json.loads(source), "test-source-hash")
        self.assertNotIn(ROOT / "docs/extensions/API-0.4-REFERENCE.md", outputs)

    def test_generate_and_check_leave_reference_untouched(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_bytes((ROOT / "Cargo.toml").read_bytes())
            source = root / "protocol/extension-api-v0.3.schema.json"
            source.parent.mkdir(parents=True)
            source.write_bytes(generator.SOURCE.read_bytes())
            reference = root / "docs/extensions/API-0.4-REFERENCE.md"
            reference.parent.mkdir(parents=True)
            sentinel = b"# Hand-maintained API 0.4 reference\nnot generated\n"
            reference.write_bytes(sentinel)
            with (
                patch.object(generator, "ROOT", root),
                patch.object(generator, "SOURCE", source),
                patch.object(generator, "FIXTURE_DIRECTORY", root / "protocol/fixtures/extension-api-v0.3"),
                patch.object(generator, "NEGATIVE_FIXTURE_DIRECTORY", root / "protocol/fixtures/extension-api-v0.3/negative"),
            ):
                # Generation still writes all of the live 0.3 outputs, and its
                # drift check succeeds independently of this reference's bytes.
                with patch.object(sys, "argv", [str(GENERATOR)]):
                    self.assertEqual(generator.main(), 0)
                self.assertEqual(reference.read_bytes(), sentinel)
                with patch.object(sys, "argv", [str(GENERATOR), "--check"]):
                    self.assertEqual(generator.main(), 0)
                self.assertEqual(reference.read_bytes(), sentinel)


if __name__ == "__main__":
    unittest.main()
