"""Background jobs stay bounded, owner-fenced and cancellable."""
import threading
from types import SimpleNamespace
import unittest
from unittest import mock

from octet_extension import tool_result, text_content
from octet_computer_use import jev_use, jev_use_jobs as jobs

CONTEXT = {'resource_owner': {'session_id': 'one', 'extension_instance_id': 'extension', 'process_generation': 1}}
OTHER = {'resource_owner': {**CONTEXT['resource_owner'], 'session_id': 'two'}}


class JobTests(unittest.TestCase):
    def setUp(self):
        jev_use.reset_shutdown_state()
        self.extension = SimpleNamespace(cancellation=None, confirm=mock.Mock(return_value=True))
        self.manager = jobs.Jobs(object(), self.extension)
        self.addCleanup(self.manager.shutdown)
        self.addCleanup(jev_use.reset_shutdown_state)

    def start_blocked(self):
        started = threading.Event()
        def work(*args, cancellation, **kwargs):
            started.set()
            cancellation.event.wait(2)
            return tool_result(text_content('stopped'), is_error=cancellation.cancelled)
        patch = mock.patch.object(jobs.binding, 'dispatch', side_effect=work)
        patch.start()
        self.addCleanup(patch.stop)
        result = self.manager.handle('setup', {}, CONTEXT)
        self.assertTrue(started.wait(1))
        return result['structured_content']['job_id']

    def test_start_returns_without_wait_and_cancel_preserves_terminal_error(self):
        identifier = self.start_blocked()
        result = self.manager.handle('status', {'job_id': identifier}, CONTEXT)
        self.assertEqual(result['structured_content']['status'], 'running')
        self.manager.handle('cancel', {'job_id': identifier}, CONTEXT)
        self.manager.jobs[identifier].thread.join(1)
        result = self.manager.handle('status', {'job_id': identifier}, CONTEXT)
        self.assertEqual(result['structured_content']['status'], 'finished')
        self.assertTrue(result['is_error'])

    def test_cross_owner_cannot_observe_or_cancel(self):
        identifier = self.start_blocked()
        for operation in ('status', 'cancel'):
            self.assertTrue(self.manager.handle(operation, {'job_id': identifier}, OTHER)['is_error'])
        self.assertFalse(self.manager.jobs[identifier].token.cancelled)

    def test_only_one_runtime_operation_at_once(self):
        self.start_blocked()
        self.assertTrue(self.manager.handle('setup', {}, CONTEXT)['is_error'])

    def test_missing_owner_fails_before_dispatch(self):
        with mock.patch.object(jobs.binding, 'dispatch') as dispatch:
            self.assertTrue(self.manager.handle('setup', {}, {})['is_error'])
            dispatch.assert_not_called()

    def test_shutdown_and_session_end_cancel(self):
        identifier = self.start_blocked()
        self.manager.session_end({'binding': CONTEXT['resource_owner']})
        self.assertTrue(self.manager.jobs[identifier].token.cancelled)
        self.manager.jobs[identifier].thread.join(1)
        self.assertTrue(self.manager.handle('setup', {}, CONTEXT)['is_error'])
        self.manager.shutdown()
        self.assertTrue(self.manager.handle('setup', {}, OTHER)['is_error'])

    def test_settlement_requires_complete_matching_owner(self):
        identifier = self.start_blocked()
        same_session_other_instance = {'session_id': 'one', 'extension_instance_id': 'other', 'process_generation': 1}
        self.manager.session_end({'binding': same_session_other_instance})
        self.assertFalse(self.manager.jobs[identifier].token.cancelled)
        self.manager.session_end({'binding': CONTEXT['resource_owner']},
                                 {'resource_owner': {**CONTEXT['resource_owner'], 'process_generation': 2}})
        self.assertFalse(self.manager.jobs[identifier].token.cancelled)
        self.manager.session_end({'binding': CONTEXT['resource_owner']})
        self.assertTrue(self.manager.jobs[identifier].token.cancelled)

    def test_manifest_negotiated_sdk_session_end_hook_cancels_owner_job(self):
        import tomllib
        from pathlib import Path
        from octet_extension import Extension

        manifest_path = Path(__file__).resolve().parents[1] / 'extension.toml'
        manifest = tomllib.loads(manifest_path.read_text(encoding='utf-8'))
        declared_hooks = manifest['contributes']['hooks']
        self.assertEqual(declared_hooks, ['session_end'])

        identifier = self.start_blocked()
        sdk = Extension(api_version='0.4')
        sdk.hook('session_end')(self.manager.session_end)
        initialized = sdk._initialize({
            'api_version': '0.4',
            'contributes': {'hooks': declared_hooks, 'tools': [], 'commands': []},
            'protocol': {'version': '0.4', 'required_features': [], 'optional_features': [],
                         'limits': {'max_concurrent_requests': 1}},
        })
        self.assertEqual(initialized['protocol']['version'], '0.4')
        self.assertEqual(sdk._dispatch('hook/run', {
            'hook': 'session_end',
            'payload': {'binding': CONTEXT['resource_owner'], 'outcome': 'completed',
                        'reason': 'session_ended', 'duration_ms': 1},
        })['disposition']['action'], 'continue')
        self.assertTrue(self.manager.jobs[identifier].token.cancelled)
        self.manager.jobs[identifier].thread.join(1)

    def test_setup_denial_never_starts_job(self):
        self.extension.confirm.return_value = False
        with mock.patch.object(jobs.binding, 'dispatch') as dispatch:
            result = self.manager.handle('setup', {}, CONTEXT, gated=True)
            self.assertTrue(result['is_error'])
            dispatch.assert_not_called()
        self.assertFalse(self.manager.jobs)

    def test_request_cancellation_is_retained_by_background_job(self):
        token = SimpleNamespace(cancelled=False)
        self.extension.cancellation = token
        identifier = self.start_blocked()
        token.cancelled = True
        self.assertTrue(self.manager.jobs[identifier].token.cancelled)

    def test_gated_run_stays_on_request_thread_and_creates_no_job(self):
        with mock.patch.object(jobs.binding, 'dispatch', return_value=tool_result(text_content('denied'), is_error=True)) as dispatch:
            self.assertTrue(self.manager.handle('run', {}, CONTEXT, gated=True)['is_error'])
        self.assertTrue(dispatch.call_args.kwargs['gated'])
        self.assertFalse(self.manager.jobs)

    def test_empty_shutdown_does_not_set_stopping_flag(self):
        self.manager.shutdown()
        self.assertFalse(jev_use._STOPPING.is_set())

    def test_active_shutdown_signals_and_sets_stopping_flag(self):
        identifier = self.start_blocked()
        self.manager.shutdown()
        self.assertTrue(jev_use._STOPPING.is_set())
        self.manager.jobs[identifier].thread.join(1)


if __name__ == '__main__':
    unittest.main()
