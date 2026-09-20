"""Test only the confirmation guard; never run removal operations."""
from pathlib import Path
import subprocess
import unittest


class RemovalGuardTests(unittest.TestCase):
    def test_confirmation_is_exactly_two_arguments(self):
        script = Path(__file__).parents[1] / "scripts" / "remove.sh"
        guard, boundary, _ = script.read_text().partition('\nTARGET=')
        self.assertEqual(boundary, '\nTARGET=', 'refusing to execute an unrecognized removal script')
        for args, status in [
            ([], 77),
            (["--confirm REMOVE-OCTET"], 77),
            (["--confirm", "WRONG"], 77),
            (["--confirm", "REMOVE-OCTET", "extra"], 77),
            (["--confirm", "REMOVE-OCTET"], 0),
        ]:
            with self.subTest(args=args):
                result = subprocess.run(
                    ["bash", "-c", guard, "remove-guard", *args],
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(result.returncode, status, result.stderr)


if __name__ == "__main__":
    unittest.main()
