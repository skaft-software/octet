"""Public-tool and command gates for the upstream recipe (no desktop access)."""
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest import mock

from octet_computer_use import entrypoint, jev_use_binding as binding


class BindingTests(unittest.TestCase):
    def setUp(self):
        self.extension = SimpleNamespace(cancellation=mock.Mock(), confirm=mock.Mock(return_value=True))
        self.computer = SimpleNamespace(_paths=object(), status=mock.Mock())

    def call(self, operation, arguments=None, gated=False):
        return binding.dispatch(operation, arguments or {}, computer=self.computer,
                                extension=self.extension, home=Path('/test-home'), gated=gated)

    def test_registered_tools_have_schemas_and_manifest_entries(self):
        extension, _ = entrypoint.create_extension()
        self.assertIn('session_end', extension._hooks)
        self.assertEqual(extension._hooks['session_end'].__self__.__class__.__name__, 'Jobs')
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
        with mock.patch.object(binding.jev_use, 'run') as run, mock.patch.object(self.computer, 'status') as status:
            result = self.call('run', gated=True)
        self.assertTrue(result['is_error'])
        run.assert_not_called()
        status.assert_not_called()

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
        with mock.patch.object(binding, 'resolve_key', return_value=None):
            self.assertTrue(self.call('run', {'live': True})['is_error'])
            self.computer.status.assert_not_called()

    def test_run_preserves_selected_runtime_and_reports_incomplete_as_error(self):
        health = {'runtime': 'desktop-host', 'runtime_binary': '/selected/cua-driver',
                  'permissions': 'granted', 'permission_detail': 'permissions ready', 'doctor_ok': True, 'platform': 'darwin'}
        self.computer.status = mock.Mock(return_value=health)
        with mock.patch.object(binding.jev_use, 'run', return_value={'complete': False, 'status': 'unknown'}) as run:
            result = self.call('run', {'max_steps': 3})
        self.assertTrue(result['is_error'])
        self.assertEqual(run.call_args.kwargs['driver_binary'], Path('/selected/cua-driver'))
        self.assertIsNone(run.call_args.kwargs['api_key'])
        self.assertEqual(run.call_args.kwargs['max_steps'], 3)

    def test_unavailable_host_does_not_fall_back(self):
        health = {'runtime': 'unavailable', 'permissions': 'granted', 'permission_detail': 'permissions ready', 'doctor_ok': True, 'platform': 'darwin'}
        self.computer.status = mock.Mock(return_value=health)
        with mock.patch.object(binding.jev_use, 'run') as run:
            self.assertTrue(self.call('run')['is_error'])
            run.assert_not_called()

    def test_unknown_permission_on_non_darwin_uses_runtime_readiness(self):
        for platform_name in ('linux', 'windows'):
            with self.subTest(platform=platform_name):
                health = {'runtime': 'direct', 'runtime_binary': '/selected/cua-driver',
                          'permissions': 'granted' if platform_name == 'linux' else 'unknown',
                          'permission_detail': 'display reachable' if platform_name == 'linux' else 'probe responded; permission state unknown',
                          'doctor_ok': True, 'platform': platform_name}
                self.computer.status = mock.Mock(return_value=health)
                with mock.patch.object(binding.jev_use, 'run', return_value={'ok': True}) as run:
                    self.call('run')
                run.assert_called_once()

    def test_denied_permission_still_blocks_recipe(self):
        health = {'runtime': 'direct', 'runtime_binary': '/selected/cua-driver',
                  'permissions': 'denied', 'permission_detail': 'no display session', 'doctor_ok': True, 'platform': 'linux'}
        self.computer.status = mock.Mock(return_value=health)
        with mock.patch.object(binding.jev_use, 'run') as run:
            self.assertTrue(self.call('run')['is_error'])
        run.assert_not_called()

    def test_live_linux_denial_from_status_prevents_recipe_dispatch(self):
        from octet_computer_use import driver

        extension, computer = entrypoint.create_extension(home=Path('/test-home'))
        computer.client = lambda: object()
        base_health = SimpleNamespace(as_dict=lambda: {
            'installed': True, 'runtime': 'direct', 'runtime_binary': '/selected/cua-driver',
            'permissions': 'unknown', 'doctor_ok': True, 'platform': 'linux'})
        with mock.patch.object(driver, 'health', return_value=base_health), \
                mock.patch.object(driver, 'permission_state', return_value={
                    'permissions': 'denied', 'detail': 'no display session'}), \
                mock.patch.object(binding.jev_use, 'run') as run:
            status = computer.status()
            self.assertEqual(status['permissions'], 'denied')
            result = binding.dispatch('run', {}, computer=computer, extension=extension,
                                      home=Path('/test-home'))
        self.assertTrue(result['is_error'])
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
