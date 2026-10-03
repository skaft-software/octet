from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock

from octet_mcp.ownership import ResourceOwner
from octet_mcp.runtime import build_runtime


class RuntimeOwnerTests(unittest.TestCase):
    def test_foreign_owner_add_http_is_rejected_before_prompt_or_config_write(self):
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "mcp.json"
            original_config = json.dumps({"version": 1, "servers": {}})
            config_path.write_text(original_config, encoding="utf-8")
            extension, manager = build_runtime(
                config_path=config_path, experimental_streamable_http_mcp=True
            )
            try:
                self.assertTrue(manager.bind_owner({"resource_owner": ResourceOwner("session-a", "instance-a", 1).wire()}))
                prompt = Mock(side_effect=AssertionError("foreign owner must be rejected before prompting"))
                extension.request_input = prompt
                result = extension._commands["mcp"].handler(
                    ["add", "http"],
                    {"resource_owner": ResourceOwner("session-b", "instance-b", 1).wire()},
                )
                self.assertIn("owner mismatch", result["text"])
                self.assertEqual(prompt.call_count, 0)
                self.assertEqual(config_path.read_text(encoding="utf-8"), original_config)
                self.assertEqual(manager.config.servers, ())
            finally:
                manager.shutdown()


if __name__ == "__main__":
    unittest.main()
