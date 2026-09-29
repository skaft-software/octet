"""Owner-scoped background jobs; the host's short RPC deadline is not a run budget."""
from __future__ import annotations

from dataclasses import dataclass, field
import threading
import time
import uuid
from typing import Any, Mapping

from octet_extension import CancelledError, text_content, tool_result
from . import jev_use_binding as binding


class JobCancellation:
    def __init__(self, request_token=None):
        self.event = threading.Event()
        self.request_token = request_token

    @property
    def cancelled(self):
        return self.event.is_set() or bool(self.request_token and self.request_token.cancelled)

    def raise_if_cancelled(self):
        if self.cancelled:
            raise CancelledError()


@dataclass
class Job:
    owner: tuple
    operation: str
    token: JobCancellation
    identifier: str = field(default_factory=lambda: uuid.uuid4().hex)
    result: Any = None
    thread: Any = None


def _owner(context):
    owner = context.get('resource_owner')
    if not isinstance(owner, Mapping):
        raise ValueError('jev-use jobs require a host resource owner')
    fields = (owner.get('session_id'), owner.get('extension_instance_id'), owner.get('process_generation'))
    if not all(isinstance(value, str) and value for value in fields[:2]) or type(fields[2]) is not int or fields[2] < 1:
        raise ValueError('jev-use jobs require a valid host resource owner')
    return fields


class Jobs:
    def __init__(self, computer, extension, home=None):
        self.computer, self.extension, self.home = computer, extension, home
        self.lock = threading.Lock()
        self.jobs: dict[str, Job] = {}
        self.closed = False
        self.ended_sessions: set[str] = set()

    def _report(self, job):
        if job.result is not None:
            detail = ' '.join(part.get('text', '') for part in job.result.get('content', []) if isinstance(part, dict))[:4000]
            return tool_result(text_content('jev-use job finished. ' + detail),
                               structured_content={'job_id': job.identifier, 'status': 'finished',
                                                   'operation': job.operation, 'result': job.result},
                               is_error=bool(job.result.get('is_error')))
        return tool_result(text_content('jev-use job running. Use computer_use_jev_use_status with this job_id; do not launch it again.'),
                           structured_content={'job_id': job.identifier, 'status': 'cancelling' if job.token.cancelled else 'running',
                                               'operation': job.operation})

    def owned(self, context):
        """This owner's jobs, newest first, as ``(job_id, operation, state)``."""
        try:
            owner = _owner(context)
        except ValueError:
            return []
        with self.lock:
            jobs = [job for job in self.jobs.values() if job.owner == owner]
            return [(job.identifier, job.operation,
                     'finished' if job.result is not None
                     else 'cancelling' if job.token.cancelled else 'running')
                    for job in reversed(jobs)]

    def handle(self, operation, values, context, *, gated=False):
        if operation == 'status' and not values:
            return binding.dispatch(operation, values, computer=self.computer, extension=self.extension, home=self.home, gated=gated)
        try:
            owner = _owner(context)
            if operation in ('status', 'cancel'):
                if set(values) != {'job_id'} or not isinstance(values['job_id'], str):
                    raise ValueError('job_id is required')
                with self.lock:
                    job = self.jobs.get(values['job_id'])
                    if job is None or job.owner != owner:
                        raise ValueError('No matching jev-use job for this owner')
                    if operation == 'cancel':
                        job.token.event.set()
                    return self._report(job)
            options = binding._validate(operation, values)
        except ValueError as error:
            return tool_result(text_content(str(error)), is_error=True)
        token = JobCancellation(self.extension.cancellation)
        token.raise_if_cancelled()
        # Resolve request-scoped UI approvals before leaving the RPC context.
        if gated and operation == 'run':
            return binding.dispatch(operation, options, computer=self.computer, extension=self.extension, home=self.home, gated=True)
        if gated and operation == 'setup':
            try:
                approved = self.extension.confirm('Install upstream jev-use?', detail='Downloads pinned Cua source and locked dependencies.', default=False)
            except Exception:
                approved = False
            if approved is not True:
                return tool_result(text_content('Denied: jev-use setup was not confirmed.'), is_error=True)
        token.raise_if_cancelled()
        with self.lock:
            if self.closed or owner[0] in self.ended_sessions:
                return tool_result(text_content('jev-use is shutting down.'), is_error=True)
            # One active recipe prevents concurrent setup/source mutation and
            # competing fixtures. Finished evidence is bounded but never deleted.
            if any(job.result is None for job in self.jobs.values()):
                return tool_result(text_content('A jev-use job is already running. Inspect or cancel its job_id first.'), is_error=True)
            if len(self.jobs) >= 16:
                del self.jobs[next(iter(self.jobs))]
            job = Job(owner, operation, token)
            self.jobs[job.identifier] = job

            def work():
                try:
                    result = binding.dispatch(operation, options, computer=self.computer, extension=self.extension,
                                              home=self.home, gated=False, cancellation=token)
                except BaseException as error:
                    result = tool_result(text_content('jev-use job stopped (' + type(error).__name__ + '). Actions may have occurred; inspect evidence before retrying.'), is_error=True)
                with self.lock:
                    job.result = result

            job.thread = threading.Thread(target=work, name='jev-use-' + job.identifier[:8], daemon=True)
            if token.cancelled:
                del self.jobs[job.identifier]
                token.raise_if_cancelled()
            try:
                job.thread.start()
            except RuntimeError:
                del self.jobs[job.identifier]
                raise
            if token.cancelled:
                job.token.event.set()
            return self._report(job)

    def session_settled(self, values, context=None):
        session_id = values.get('session_id') if isinstance(values, Mapping) else None
        if not isinstance(session_id, str):
            return
        with self.lock:
            if len(self.ended_sessions) >= 1024:
                self.closed = True
            self.ended_sessions.add(session_id)
            for job in self.jobs.values():
                if job.owner[0] == session_id:
                    job.token.event.set()

    def shutdown(self):
        """Cancel owned jobs and signal owned subprocesses.

        Extension shutdown is terminal for the process (≈2s host deadline).
        This signals job tokens immediately, signals owned subprocesses
        without waiting, then joins worker threads for at most 0.5s total so
        a hung driver cannot stall exit. It does not guarantee tree cleanup:
        POSIX descendants that already reparented cannot be reclaimed by PID
        scan, and ``cancel_all_processes`` reports ``cleanup_complete: False``.
        Windows Job-Object/suspended-launch paths are unexercised here.
        Empty shutdowns (no unfinished jobs and no active subprocesses) do
        not set the process-wide stopping flag.
        """
        with self.lock:
            self.closed = True
            jobs = list(self.jobs.values())
            for job in jobs:
                job.token.event.set()
            active = any(job.result is None for job in jobs)
        if active or binding.jev_use._ACTIVE_PROCESSES:
            binding.jev_use.cancel_all_processes()
        deadline = time.monotonic() + 0.5
        for job in jobs:
            if job.thread:
                job.thread.join(timeout=max(0, deadline - time.monotonic()))
