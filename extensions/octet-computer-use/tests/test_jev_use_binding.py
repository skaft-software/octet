"""Public-tool and command gates for the upstream recipe (no desktop access)."""
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest import mock

from octet_computer_use import entrypoint, jev_use_binding as binding


class BindingTests(unittest.TestCase):
    def setUp(self):
        self.extension = SimpleNamespace(cancellation=mock.Mock(), confirm=mock.Mock(return_value=True))
        self.computer = SimpleNamespace(_paths=object())

    def call(self, operation, arguments=None, gated=False):
        return binding.dispatch(operation, arguments or {}, computer=self.computer,
                                extension=self.extension, home=Path('/test-home'), gated=gated)

    def test_registered_tools_have_schemas_and_manifest_entries(self):
        extension, _ = entrypoint.create_extension()
        manifest = (Path(__file__).parents[1] / 'extension.toml').read_text()
        for operation in binding.TOOLS:
            name = binding.PREFIX + operation
            self.assertIn(name, extension._tools)
            self.assertIn('"' + name + '"', manifest)

    def test_status_is_inert_and_forwards_home(self):
        with mock.patch.object(binding.jev_use, 'status', return_value={'status': 'unavailable'}) as status:
            self.call('status')
        status.assert_called_once_with(home=Path('/test-home'))
        self.extension.confirm.assert_not_called()

    def test_gated_run_never_starts_runner_or_probes_driver(self):
        with mock.patch.object(binding.jev_use, 'run') as run, mock.patch.object(binding.driver, 'health') as health:
            result = self.call('run', gated=True)
        self.assertTrue(result['is_error'])
        run.assert_not_called()
        health.assert_not_called()

    def test_setup_denial_and_unavailable_confirmation_fail_closed(self):
        for answer in (False, None):
            self.extension.confirm.return_value = answer
            with mock.patch.object(binding.jev_use, 'setup') as setup:
                self.assertTrue(self.call('setup', gated=True)['is_error'])
                setup.assert_not_called()
        self.extension.confirm.side_effect = RuntimeError('unavailable')
        with mock.patch.object(binding.jev_use, 'setup') as setup:
            self.assertTrue(self.call('setup', gated=True)['is_error'])
            setup.assert_not_called()

    def test_invalid_options_do_not_reach_runtime(self):
        for options in ({'live': 'yes'}, {'max_steps': True}, {'max_steps': 33},
                        {'url': 'https://example.com'}, {'require_visual_path': True},
                        {'visual_observation': 'guess'}):
            with mock.patch.object(binding.jev_use, 'run') as run:
                self.assertTrue(self.call('run', options)['is_error'])
                run.assert_not_called()

    def test_missing_key_stops_before_browser_preflight(self):
        with mock.patch.object(binding, 'resolve_key', return_value=None), mock.patch.object(binding.driver, 'health') as health:
            self.assertTrue(self.call('run', {'live': True})['is_error'])
            health.assert_not_called()

    def test_run_preserves_selected_runtime_and_reports_incomplete_as_error(self):
        health = SimpleNamespace(as_dict=lambda: {'runtime': 'desktop-host', 'runtime_binary': '/selected/cua-driver', 'permissions': 'granted'})
        with mock.patch.object(binding.driver, 'health', return_value=health), mock.patch.object(binding.jev_use, 'run', return_value={'complete': False, 'status': 'unknown'}) as run:
            result = self.call('run', {'max_steps': 3})
        self.assertTrue(result['is_error'])
        self.assertEqual(run.call_args.kwargs['driver_binary'], Path('/selected/cua-driver'))
        self.assertIsNone(run.call_args.kwargs['api_key'])
        self.assertEqual(run.call_args.kwargs['max_steps'], 3)

    def test_unavailable_host_does_not_fall_back(self):
        health = SimpleNamespace(as_dict=lambda: {'runtime': 'unavailable', 'permissions': 'granted'})
        with mock.patch.object(binding.driver, 'health', return_value=health), mock.patch.object(binding.jev_use, 'run') as run:
            self.assertTrue(self.call('run')['is_error'])
            run.assert_not_called()

    def test_mock_chooser_does_not_load_key(self):
        request = {'schema': 'cua.jev_choice_request_v1'}
        with mock.patch.object(binding, 'resolve_key') as key, mock.patch.object(binding.jev_use, 'choose', return_value={'schema': 'cua.jev_choice_v1'}) as choose:
            self.call('choose', {'request': request, 'mock': True})
        key.assert_not_called()
        self.assertEqual(choose.call_args.args, (request,))
        self.assertIsNone(choose.call_args.kwargs['api_key'])

    def test_errors_do_not_echo_secret(self):
        with mock.patch.object(binding.jev_use, 'setup', side_effect=RuntimeError('secret-key')):
            self.assertNotIn('secret-key', str(self.call('setup')))

    def test_cancelled_call_does_not_start_work(self):
        self.extension.cancellation.raise_if_cancelled.side_effect = RuntimeError('cancelled')
        with mock.patch.object(binding.jev_use, 'setup') as setup:
            with self.assertRaisesRegex(RuntimeError, 'cancelled'):
                self.call('setup')
            setup.assert_not_called()

    def test_command_grammar_is_exact(self):
        operation, options = binding.command_options(['run', '--live', '--typescript', '--max-steps', '4'])
        self.assertEqual(operation, 'run')
        self.assertEqual(options, {'live': True, 'typescript': True, 'max_steps': 4})
        for args in (['status', '--live'], ['setup', '--live'], ['run', '--url', 'https://x'],
                     ['run', '--live', '--live'], ['run', '--max-steps', '100'], ['choose']):
            with self.assertRaises(ValueError):
                binding.command_options(args)

    def test_command_routes_to_same_binding(self):
        extension, computer = entrypoint.create_extension(home=Path('/test-home'))
        with mock.patch.object(entrypoint.jev_use_jobs.Jobs, 'handle', return_value={
                'content': [{'type': 'text', 'text': 'ran'}]}) as dispatch:
            result = extension._commands['computer-use'].handler(['jev-use', 'run', '--live'], {})
        self.assertEqual(result, {'text': 'ran'})
        self.assertEqual(dispatch.call_args.args, ('run', {'live': True}, {}))


if __name__ == '__main__':
    unittest.main()
