"""Typed source SDK example: local native resources, real ngspice, immutable bulk."""
import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import sys
from typing import Literal

# Source recipe, not a compatibility shim or package installation.
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "sdk" / "python"))
from octet_extension import BlobRef, Diagnostic, Extension, Resource, TypedResult

from solver import (Circuit, SimulationSession, SpiceError, Measurement, MAX_SAMPLES,
                    SAMPLE, MEDIA_TYPE, measure, require_ngspice)

EVENT_LOG = None  # Optional harness-owned append-only file, never tool input.
INTERRUPT_BARRIER = None  # F02 only; never model-selected.


def record(event, solver_pid=0):
    if EVENT_LOG is not None:
        line = (json.dumps({"pid": os.getpid(), "event": event, "solver_pid": solver_pid}) + "\n").encode()
        if EVENT_LOG.tell() + len(line) > 64 * 1024:
            raise SpiceError("spice.log_limit", "Harness event log exceeded its 64 KiB bound.")
        EVENT_LOG.write(line)
        EVENT_LOG.flush()


def solver_event(event, solver_pid):
    record(event, solver_pid)
    if event == "solver_started" and INTERRUPT_BARRIER is not None:
        from interrupt_probe import hold_until_cancelled
        hold_until_cancelled(solver_pid, ext.cancellation, INTERRUPT_BARRIER, record)


def dispose(native):
    native.close()
    record("disposed_" + type(native).__name__)


ext = Extension(api_version="0.4", max_concurrent_requests=1,
                supported_features=("request_cancellation", "content_parts", "request_progress"))
# Each native nominal type is declared once. Schemas and resource slots derive
# from Resource[T] annotations below; no handwritten descriptors or RPC handlers.
ext.resource_type("spice.Circuit.v1", dispose=dispose)(Circuit)
ext.resource_type("spice.SimulationSession.v1", dispose=dispose)(SimulationSession)


@dataclass
class OpenInput:
    netlist: Literal["rc.cir"] = "rc.cir"


@dataclass
class CircuitRef:
    circuit: Resource[Circuit]


@dataclass
class SessionRef:
    session: Resource[SimulationSession]


@dataclass
class WaveformRef:
    # Domain profile over BlobRef, not a kernel/native resource or a locator.
    blob: BlobRef
    samples: int
    encoding: Literal["f64le-interleaved-time-voltage.v1"] = "f64le-interleaved-time-voltage.v1"
    signal: Literal["v(out)"] = "v(out)"
    time_unit: Literal["s"] = "s"
    value_unit: Literal["V"] = "V"

    def summary(self) -> str:
        # Structured details alone are not model text. Project only the descriptor.
        return f"RC transient ({self.samples} samples): {json.dumps(self.blob.to_wire())}"


def failure(error: SpiceError):
    return TypedResult(None, "SPICE operation failed.", is_error=True,
                       diagnostics=(Diagnostic("error", error.code, str(error)),))


@ext.typed_tool(name="spice_open", description="Open the bundled deterministic RC netlist")
def open_circuit(args: OpenInput) -> TypedResult[CircuitRef]:
    record("spice_open")
    ext.cancellation.raise_if_cancelled()
    try:
        circuit = Circuit()
    except SpiceError as error:
        return failure(error)
    try:
        reference = ext.export(circuit)
    except BaseException:
        circuit.close()  # Failed registration did not transfer native ownership.
        raise
    return TypedResult(CircuitRef(reference), f"Opened RC circuit: {json.dumps(reference.to_wire())}")


@ext.typed_tool(name="spice_instantiate", description="Instantiate a native simulation session",
                receiver="/circuit",
                summary=lambda value: f"Instantiated RC session: {json.dumps(value.session.to_wire())}")
def instantiate(args: CircuitRef) -> SessionRef:
    record("spice_instantiate")
    ext.cancellation.raise_if_cancelled()
    session = SimulationSession(args.circuit.value)
    try:
        return SessionRef(ext.export(session))
    except BaseException:
        session.close()
        raise


@ext.typed_tool(name="spice_transient", description="Run the RC transient with real ngspice",
                receiver="/session")
def transient(args: SessionRef) -> TypedResult[WaveformRef]:
    record("spice_transient")
    check = ext.cancellation.raise_if_cancelled
    try:
        data = args.session.value.transient(check, lambda text: ext.progress(message=text), solver_event)
        check()
        blob = ext.bulk.publish_bytes(data, media_type=MEDIA_TYPE)
        record("waveform_committed_provisionally")
        check()
        waveform = WaveformRef(blob, len(data) // SAMPLE.size)
        return TypedResult(waveform, waveform.summary())
    except SpiceError as error:
        return failure(error)


@ext.typed_tool(name="spice_measure", description="Measure a BlobRef-backed RC waveform")
def measure_waveform(waveform: WaveformRef) -> TypedResult[Measurement]:
    record("spice_measure")
    ext.cancellation.raise_if_cancelled()
    try:
        if (not 2 <= waveform.samples <= MAX_SAMPLES
                or waveform.blob.bytes != waveform.samples * SAMPLE.size
                or waveform.blob.media_type != MEDIA_TYPE):
            raise SpiceError("spice.waveform", "Waveform descriptor does not match the bounded RC profile.")
        # The SDK verifies immutable bytes and closes both file and lease on exit.
        with ext.bulk.read(waveform.blob, max_bytes=waveform.blob.bytes) as stream:
            result = measure(stream.read())
        ext.cancellation.raise_if_cancelled()
        return TypedResult(result, f"Final RC voltage: {result.final_voltage_v:.6f} V at 5 ms.")
    except SpiceError as error:
        return failure(error)


@ext.on_shutdown
def shutdown(params):
    record("shutdown")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--events", help="new private host-harness event log (64 KiB maximum)")
    parser.add_argument("--interrupt-barrier", type=Path,
                        help="F02 harness directory; SIGSTOP real ngspice after exec until cancellation")
    args = parser.parse_args()
    try:
        require_ngspice()
    except SpiceError as error:
        print(error, file=sys.stderr)
        raise SystemExit(2)
    if args.interrupt_barrier is not None:
        if not args.events or not args.interrupt_barrier.is_absolute() or not args.interrupt_barrier.is_dir():
            parser.error("interrupt barrier requires an existing absolute private directory and --events")
        INTERRUPT_BARRIER = args.interrupt_barrier
    if args.events:
        try:
            descriptor = os.open(args.events, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_APPEND, 0o600)
            EVENT_LOG = os.fdopen(descriptor, "ab")
        except OSError:
            print("BLOCKED: cannot create private harness event log.", file=sys.stderr)
            raise SystemExit(2)
    try:
        record("started")
        ext.run()
    finally:
        if EVENT_LOG is not None:
            EVENT_LOG.close()
