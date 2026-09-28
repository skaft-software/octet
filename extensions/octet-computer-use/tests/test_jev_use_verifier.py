"""The fixed adapter forwards runner options without changing verification."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class VerifierTests(unittest.TestCase):
    def test_modes_reach_both_runners_and_other_arguments_reach_verifier(self):
        script = Path(__file__).parents[1] / 'octet_computer_use' / 'jev_use_verifier.py'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'verify_setup.py').write_text('''
import json, sys

def runner_command(language, provider):
    return [language, '--provider', provider]

def main():
    print(json.dumps({'python': runner_command('python', 'mock'),
                      'typescript': runner_command('typescript', 'live'),
                      'args': sys.argv[1:]}))
''')
            for mode in ('auto', 'always', 'off'):
                result = subprocess.run([sys.executable, str(script.resolve()), '--visual-observation', mode,
                                         '--max-steps', '4', '--output-dir', 'proof'], cwd=root,
                                        capture_output=True, text=True, timeout=10, check=True)
                data = json.loads(result.stdout)
                self.assertEqual(data['python'], ['python', '--provider', 'mock', '--visual-observation', mode])
                self.assertEqual(data['typescript'], ['typescript', '--provider', 'live', '--visual-observation', mode])
                self.assertEqual(data['args'], ['--max-steps', '4', '--output-dir', 'proof'])

    def test_invalid_mode_is_rejected_before_import(self):
        script = Path(__file__).parents[1] / 'octet_computer_use' / 'jev_use_verifier.py'
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([sys.executable, str(script.resolve()), '--visual-observation', 'guess'],
                                    cwd=directory, capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('FileNotFoundError', result.stderr)


if __name__ == '__main__':
    unittest.main()
