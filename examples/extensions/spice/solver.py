"""Small, real ngspice backend. No SDK, shell, arbitrary netlists or fallback data."""
from __future__ import annotations

from dataclasses import dataclass
import math
import os
from pathlib import Path
import selectors
import shutil
import struct
import subprocess
import tempfile
import time
from typing import Callable

MAX_SOURCE_BYTES = 4096
MAX_OUTPUT_BYTES = 1024 * 1024  # Combined stdout/stderr, never copied to tool text.
MAX_SAMPLES = 10_000
SAMPLE = struct.Struct("<dd")  # Interleaved time (s), voltage (V).
MAX_WAVEFORM_BYTES = MAX_SAMPLES * SAMPLE.size
MEDIA_TYPE = "application/octet-stream"
STOP_S = 0.005
TAU_S = 0.001


class SpiceError(Exception):
    """Only fixed, bounded messages are exposed to the model."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def require_ngspice() -> str:
    if os.name != "posix":
        raise SpiceError("spice.prerequisite", "BLOCKED: this source example requires POSIX pipe selectors.")
    executable = shutil.which("ngspice")
    if executable is None:
        raise SpiceError("spice.prerequisite", "BLOCKED: real ngspice executable is missing; no fallback.")
    return str(Path(executable).resolve())


class Circuit:
    """Native object; only its host-issued ResourceRef crosses the protocol."""

    def __init__(self):
        try:
            with Path(__file__).with_name("rc.cir").open("rb") as source:
                data = source.read(MAX_SOURCE_BYTES + 1)
            if len(data) > MAX_SOURCE_BYTES:
                raise ValueError("source bound")
            self.netlist = data.decode("ascii")
        except (OSError, ValueError) as error:
            raise SpiceError("spice.source", "Bundled RC netlist is unavailable or invalid.") from error

    def close(self) -> None:
        self.netlist = ""


class SimulationSession:
    """Independent native state: shares only immutable source text, not Circuit."""

    def __init__(self, circuit: Circuit):
        self.netlist = circuit.netlist
        self.completed_runs = 0

    def close(self) -> None:
        # No child can outlive transient's finally block, including cancellation.
        self.netlist = ""

    def transient(self, check_cancelled: Callable[[], None],
                  progress: Callable[[str], None], event: Callable[[str, int], None]) -> bytes:
        check_cancelled()
        executable = require_ngspice()
        progress("Starting ngspice RC transient.")
        # This call owns the directory, input file, pipe, and child through wait().
        # .print writes to a bounded pipe: no solver-created waveform/log files.
        try:
            with tempfile.TemporaryDirectory(prefix="octet-spice-") as directory:
                root = Path(directory)
                (root / "rc.cir").write_text(self.netlist, encoding="ascii")
                output = _run(executable, root, check_cancelled, event)
            check_cancelled()
            waveform = parse_print(output)
            check_cancelled()
        except OSError as error:
            raise SpiceError("spice.io", "ngspice process or private scratch I/O failed.") from error
        self.completed_runs += 1
        progress("ngspice finished; validating waveform for bulk publication.")
        return waveform


def _stop_and_wait(child: subprocess.Popen) -> None:
    if child.poll() is None:
        try:
            child.terminate()
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            child.kill()
    # A terminal result is forbidden until owned native execution has settled.
    child.wait()


def _run(executable: str, directory: Path, check_cancelled: Callable[[], None],
         event: Callable[[str, int], None]) -> bytes:
    # POSIX pipe selectors are intentional; this source recipe targets macOS/Linux.
    # -n suppresses user/system spinit; fixed rc.cir has no .control/.include effects.
    check_cancelled()
    child = subprocess.Popen([executable, "-n", "-b", "rc.cir"], cwd=directory,
                             env={**os.environ, "LC_ALL": "C"},
                             stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, shell=False)
    output = bytearray()
    deadline = time.monotonic() + 30.0
    try:
        event("solver_started", child.pid)
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while selector.get_map() or child.poll() is None:
                check_cancelled()
                if time.monotonic() >= deadline:
                    raise SpiceError("spice.timeout", "ngspice exceeded the 30-second operation bound.")
                for key, _ in selector.select(timeout=0.05):
                    chunk = os.read(key.fd, min(4096, MAX_OUTPUT_BYTES + 1 - len(output)))
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output.extend(chunk)
                    if len(output) > MAX_OUTPUT_BYTES:
                        raise SpiceError("spice.output_limit", "ngspice exceeded the output bound.")
            check_cancelled()
            if child.wait() != 0:
                raise SpiceError("spice.solver_failed", "ngspice reported an unsuccessful transient.")
            return bytes(output)
    finally:
        try:
            _stop_and_wait(child)
        finally:
            child.stdout.close()
        event("solver_settled", child.pid)


def _invalid_waveform() -> SpiceError:
    return SpiceError("spice.waveform", "Expected a bounded, finite RC transient with time and v(out).")


def parse_print(output: bytes) -> bytes:
    """Decode ngspice's paginated `.print tran v(out)` text locally, never as RPC."""
    if len(output) > MAX_OUTPUT_BYTES:
        raise _invalid_waveform()
    try:
        lines = output.decode("ascii").splitlines()
    except UnicodeError as error:
        raise _invalid_waveform() from error
    waveform = bytearray()
    header_seen = False
    for line in lines:
        columns = line.split()
        if columns == ["Index", "time", "v(out)"]:
            header_seen = True
            continue
        if not columns or not columns[0].isdigit():
            continue  # Title, repeated page headings, separators and solver stats.
        if not header_seen or len(columns) != 3:
            raise _invalid_waveform()
        try:
            index = int(columns[0])
            seconds, volts = float(columns[1]), float(columns[2])
        except ValueError as error:
            raise _invalid_waveform() from error
        if index != len(waveform) // SAMPLE.size or index >= MAX_SAMPLES:
            raise _invalid_waveform()
        waveform.extend(SAMPLE.pack(seconds, volts))
    # Validate time order, completion and RC physical plausibility before publication.
    result = bytes(waveform)
    measure(result)
    return result


@dataclass(frozen=True)
class Measurement:
    samples: int
    final_time_s: float
    final_voltage_v: float
    expected_voltage_v: float
    absolute_error_v: float


def measure(waveform: bytes) -> Measurement:
    """Only finite scalar measurements leave the extension, never these sample bytes."""
    if not 2 * SAMPLE.size <= len(waveform) <= MAX_WAVEFORM_BYTES or len(waveform) % SAMPLE.size:
        raise _invalid_waveform()
    previous_time, previous_voltage = -1.0, -1.0
    for seconds, volts in SAMPLE.iter_unpack(waveform):
        if (not math.isfinite(seconds) or not math.isfinite(volts)
                or not previous_time < seconds <= STOP_S + 1e-9
                or seconds < 0.0 or not -1e-6 <= volts <= 1.000001
                or volts < previous_voltage - 1e-6):
            raise _invalid_waveform()
        previous_time, previous_voltage = seconds, volts
    expected = 1.0 - math.exp(-STOP_S / TAU_S)
    error = abs(previous_voltage - expected)
    if abs(previous_time - STOP_S) > 1e-9 or error > 0.002:
        raise _invalid_waveform()
    return Measurement(len(waveform) // SAMPLE.size, previous_time, previous_voltage, expected, error)
