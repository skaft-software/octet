"""Process-level API 0.4 handshake and real oxi renderer smoke."""

import base64
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parent


class SnapcompactProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        subprocess.run(["cargo", "build", "--release", "--locked", "--quiet",
                        "--manifest-path", str(ROOT / "renderer" / "Cargo.toml")],
                       check=True, timeout=180)

    def test_negotiation_png_and_clean_shutdown(self):
        child = subprocess.Popen([str(ROOT / "extension.py")], cwd=ROOT,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        try:
            def request(id_, method, params):
                child.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id_,
                                              "method": method, "params": params}) + "\n")
                child.stdin.flush()
                response = json.loads(child.stdout.readline())
                self.assertEqual(response["id"], id_)
                return response

            init = request(1, "initialize", {
                "api_version": "0.4",
                "contributes": {"tools": [], "commands": [],
                                "hooks": ["compaction_strategy"]},
                "protocol": {"version": "0.4",
                             "required_features": ["request_cancellation", "content_parts"],
                             "optional_features": ["compaction_strategy"],
                             "limits": {"max_concurrent_requests": 1}},
            })
            self.assertEqual(init["result"]["protocol"]["version"], "0.4")
            self.assertIn("compaction_strategy", init["result"]["protocol"]["features"])
            result = request(2, "hook/run", {
                "hook": "compaction_strategy",
                "payload": {"model_id": "claude-sonnet", "text": "User: hello\nAssistant: hi"},
                "context": {},
            })["result"]
            frames = result["compaction_frames"]
            self.assertTrue(frames)
            self.assertTrue(base64.b64decode(frames[0]).startswith(b"\x89PNG\r\n\x1a\n"))
            invalid = request(3, "hook/run", {
                "hook": "compaction_strategy", "payload": {"model_id": "claude", "text": ""},
                "context": {},
            })
            self.assertIn("error", invalid)
            self.assertIn("result", request(4, "shutdown", {}))
            child.stdin.close()
            self.assertEqual(child.wait(timeout=5), 0)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            for stream in (child.stdin, child.stdout, child.stderr):
                if stream and not stream.closed:
                    stream.close()


if __name__ == "__main__":
    unittest.main()
