"""Opt-in F02 rendezvous, not a simulator: suspend the actual post-exec child.

Normal runs never import this module. An early real solver exit is BLOCKED,
never a substitute successful cancellation. No numerical output is manufactured.
"""
import os
import signal
import time

from solver import SpiceError


def hold_until_cancelled(pid, cancellation, directory, record):
    stopped = False
    try:
        try:
            os.kill(pid, signal.SIGSTOP)
            _, status = os.waitpid(pid, os.WUNTRACED)
        except ProcessLookupError:
            status = 0
        if not os.WIFSTOPPED(status):
            record("interrupt_unavailable", pid)
            raise SpiceError("spice.interrupt_unavailable",
                             "BLOCKED: ngspice exited before the native interruption barrier.")
        stopped = True
        record("interrupt_ready", pid)
        if not cancellation.wait(5.0):
            raise SpiceError("spice.barrier_timeout", "Host did not cancel the interruption probe.")
        record("interrupt_cancelled", pid)
        # Host now asserts resource_busy while caller cancellation has completed.
        # Poll an explicit host-owned marker, not a sleep to guess race ordering.
        deadline = time.monotonic() + 5.0
        while not (directory / "allow_stop").is_file():
            if time.monotonic() >= deadline:
                raise SpiceError("spice.barrier_timeout", "Host did not release the interruption probe.")
            time.sleep(0.005)
        cancellation.raise_if_cancelled()
    finally:
        if stopped:
            # Resume before solver.py's finally terminates/waits; SIGTERM alone
            # cannot settle a stopped process. The caller still owns this child.
            try:
                os.kill(pid, signal.SIGCONT)
            except ProcessLookupError:
                pass
