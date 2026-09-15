#!/usr/bin/env python3
"""API 0.3 process entry point for the source-only computer-use boundary.

This entry point deliberately has no desktop, browser, screenshot, filesystem,
process, or network implementation. A runtime owner may import
``ComputerUseExtension`` and inject a host policy adapter plus a backend
callback. When run as a standalone extension, every action is denied closed.
"""

from __future__ import annotations

import os
import sys
import threading
import time
from typing import Any, BinaryIO, Callable, Dict, Mapping, Optional

from octet_computer_use.policy import (
    ActionRequest,
    Decision,
    PolicyDenied,
    PolicyGate,
    TargetIdentity,
    make_action_request,
)
from octet_computer_use.protocol import (
    API_VERSION,
    ProtocolFailure,
    ProtocolState,
    canonical_bytes,
    error_response,
    failure,
    parse_envelope,
    read_frame,
    success_response,
    validate_cancel_params,
    validate_rpc_id,
    validate_shutdown_params,
    validate_tool_call_params,
    validate_tool_call_result,
    write_frame,
)

TOOL_NAME = "computer_use"
OPERATIONS = [
    "observe",
    "start",
    "click",
    "double_click",
    "drag",
    "move",
    "scroll",
    "keypress",
    "type",
    "wait",
    "screenshot",
]

TOOL_DEFINITION: Dict[str, Any] = {
    "name": TOOL_NAME,
    "description": (
        "Request one host-authorized computer-use operation. The host owns "
        "target identity, policy, trust, cancellation, and backend dispatch."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "operation": {"type": "string", "enum": OPERATIONS},
            "arguments": {"type": "object"},
        },
        "required": ["operation", "arguments"],
        "additionalProperties": False,
    },
    "output_schema": {
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["completed", "denied", "cancelled"]},
            "operation": {"type": "string"},
            "result": {},
        },
        "required": ["status", "operation"],
        "additionalProperties": False,
    },
}

# This is deliberately a small callback boundary. It is not a transport or a
# credential provider; the runtime owns the backend and receives cancellation
# provenance explicitly.
Dispatcher = Callable[[ActionRequest, Any, threading.Event], Mapping[str, Any]]


class ActiveCall:
    def __init__(self) -> None:
        self.cancelled = threading.Event()
        self.reason = "request cancelled"


class ComputerUseExtension:
    """Canonical API 0.3 server with policy/backend dependencies injected.

    The default process has neither dependency and therefore cannot execute an
    action. ``dispatcher`` must not be used to smuggle provider credentials or
    transport authority; it is an owner-controlled local backend callback.
    """

    def __init__(
        self,
        stdin: BinaryIO,
        stdout: BinaryIO,
        *,
        policy_gate: Optional[PolicyGate] = None,
        dispatcher: Optional[Dispatcher] = None,
    ) -> None:
        self.stdin = stdin
        self.stdout = stdout
        self.state = ProtocolState()
        self.policy_gate = policy_gate or PolicyGate()
        self.dispatcher = dispatcher
        self.stop_event = threading.Event()
        self.output_lock = threading.Lock()
        self.active_lock = threading.Lock()
        self.active: Dict[Any, ActiveCall] = {}
        self.workers = []  # type: ignore[var-annotated]

    def diagnostic(self, message: str) -> None:
        print(f"octet-computer-use: {message}", file=sys.stderr, flush=True)

    def send(self, value: Any) -> None:
        frame = canonical_bytes(value)
        if len(frame) > self.state.max_frame_bytes:
            raise failure("resource_exhausted", "outbound frame exceeds max_frame_bytes")
        with self.output_lock:
            write_frame(self.stdout, value, self.state.max_frame_bytes)

    def send_error(self, request_id: Any, error: ProtocolFailure) -> None:
        try:
            self.send(error_response(request_id, error.name, error.data))
        except (BrokenPipeError, OSError, ProtocolFailure) as send_error:
            self.diagnostic(f"could not send {error.name}: {send_error}")
            self.stop_event.set()

    def _context(self, value: Any) -> Dict[str, Any]:
        """Parse host-sanitized owner context; model arguments never fill it."""
        if not isinstance(value, dict):
            raise failure("invalid_params", "computer tool context must be an object")
        allowed = {
            "session_id",
            "owner_id",
            "extension_generation",
            "frame_generation",
            "scope",
            "target",
            "trusted_data_classes",
            "parent_request_id",
            "approval_token",
            "destination",
        }
        if set(value) - allowed:
            raise failure("invalid_params", "computer tool context has unknown authority fields")
        required = {
            "session_id",
            "owner_id",
            "extension_generation",
            "frame_generation",
            "scope",
            "target",
            "parent_request_id",
        }
        if required - set(value):
            raise failure("invalid_params", "computer tool context is missing host identity")
        target = TargetIdentity.from_wire(value["target"])
        trusted = value.get("trusted_data_classes", [])
        if not isinstance(trusted, list) or len(trusted) > 32:
            raise failure("invalid_params", "trusted_data_classes must be a bounded array")
        if any(not isinstance(item, str) or not item for item in trusted):
            raise failure("invalid_params", "trusted_data_classes contains an invalid value")
        # Approval is a host retry input, not an authority supplied by the
        # model. It is consumed only by PolicyGate's exact pending binding.
        approval_token = value.get("approval_token")
        if approval_token is not None:
            if not isinstance(approval_token, str) or len(approval_token) != 64:
                raise failure("invalid_params", "approval_token is malformed")
            try:
                int(approval_token, 16)
            except ValueError as error:
                raise failure("invalid_params", "approval_token is malformed") from error
            if approval_token.lower() != approval_token:
                raise failure("invalid_params", "approval_token is malformed")
        destination = value.get("destination")
        if destination is not None and not isinstance(destination, str):
            raise failure("invalid_params", "destination must be a string")
        parent_request_id = value["parent_request_id"]
        validate_rpc_id(parent_request_id)
        return {
            "session_id": value["session_id"],
            "owner_id": value["owner_id"],
            "extension_generation": value["extension_generation"],
            "frame_generation": value["frame_generation"],
            "scope": value["scope"],
            "target": target,
            "trusted_data_classes": trusted,
            "parent_request_id": parent_request_id,
            "approval_token": approval_token,
            "destination": destination,
        }

    def _denied(self, operation: str, reason: str) -> Dict[str, Any]:
        return {
            "content": [{"type": "text", "text": reason}],
            "is_error": True,
            "metadata": {"status": "denied", "operation": operation},
            "structured_content": {"status": "denied", "operation": operation},
        }

    def _cancelled(self, operation: str, reason: str) -> Dict[str, Any]:
        return {
            "content": [{"type": "text", "text": reason}],
            "is_error": True,
            "metadata": {"status": "cancelled", "operation": operation},
            "structured_content": {"status": "cancelled", "operation": operation},
        }

    def handle_tool_call(self, params: Any, cancel_event: Optional[threading.Event] = None) -> Dict[str, Any]:
        """Validate, authorize, and optionally pass one action to the backend."""
        params = validate_tool_call_params(params)
        if params["name"] != TOOL_NAME:
            raise failure("unknown_method", f"tool {params['name']!r} is not declared")
        arguments = params["arguments"]
        if not isinstance(arguments, dict) or set(arguments) != {"operation", "arguments"}:
            raise failure("invalid_params", "computer_use arguments have an invalid shape")
        operation = arguments["operation"]
        action_arguments = arguments["arguments"]
        if operation not in OPERATIONS:
            raise failure("invalid_params", "computer_use.operation is unsupported")
        if not isinstance(action_arguments, dict):
            raise failure("invalid_params", "computer_use.arguments must be an object")
        context = self._context(params["context"])
        if cancel_event is not None and cancel_event.is_set():
            return self._cancelled(operation, "request cancelled before policy evaluation")
        try:
            action = make_action_request(
                operation=operation,
                target=context["target"],
                session_id=context["session_id"],
                owner_id=context["owner_id"],
                extension_generation=context["extension_generation"],
                frame_generation=context["frame_generation"],
                arguments=action_arguments,
                scope=context["scope"],
                trusted_data_classes=context["trusted_data_classes"],
                destination=context["destination"],
            )
        except (TypeError, ValueError) as error:
            raise failure("invalid_params", str(error)) from error
        decision = self.policy_gate.evaluate(
            action,
            parent_request_id=context["parent_request_id"],
            approval_token=context["approval_token"],
        )
        if decision.decision != Decision.ALLOW:
            return self._denied(operation, decision.reason)
        if self.dispatcher is None:
            return self._denied(operation, "computer-use backend is not available")
        if cancel_event is not None and cancel_event.is_set():
            return self._cancelled(operation, "request cancelled before dispatch")
        try:
            # Consume the one-use local guard immediately before dispatch. The
            # backend still receives the cancellation event and must stop at
            # its next safe boundary.
            self.policy_gate.authorize(action, decision)
            result = self.dispatcher(action, action_arguments, cancel_event or threading.Event())
        except PolicyDenied as error:
            return self._denied(operation, str(error))
        if cancel_event is not None and cancel_event.is_set():
            return self._cancelled(operation, "request cancelled during dispatch")
        if not isinstance(result, Mapping):
            raise failure("internal_error", "computer-use backend returned a non-object result")
        result = dict(result)
        validate_tool_call_result(result)
        return result

    def _run_tool(self, request_id: Any, call: ActiveCall, params: Any) -> None:
        try:
            result = self.handle_tool_call(params, call.cancelled)
            with self.active_lock:
                current = self.active.pop(request_id, None)
            if current is None:
                return
            self.send(success_response(request_id, result))
        except ProtocolFailure as error:
            with self.active_lock:
                current = self.active.pop(request_id, None)
            if current is None:
                return
            error.request_id = request_id
            self.send_error(request_id, error)
        except Exception as error:  # pragma: no cover - process boundary guard
            with self.active_lock:
                current = self.active.pop(request_id, None)
            if current is None:
                return
            self.diagnostic(f"tool call failed: {error}")
            self.send_error(request_id, failure("internal_error", request_id=request_id))

    def _start_tool(self, params: Any, request_id: Any) -> None:
        self.state.require_method("tool/call")
        validate_tool_call_params(params)
        with self.active_lock:
            if request_id in self.active:
                raise failure("invalid_request", "request id is already active", request_id=request_id)
            if self.state.contract is None:
                raise failure("invalid_request", "contract is unavailable", request_id=request_id)
            limit = self.state.contract["limits"]["max_concurrent_requests"]
            if len(self.active) >= limit:
                raise failure("resource_exhausted", "max_concurrent_requests is exhausted", request_id=request_id)
            call = ActiveCall()
            self.active[request_id] = call
        worker = threading.Thread(
            target=self._run_tool,
            args=(request_id, call, params),
            name=f"octet-computer-use-{request_id}",
            daemon=True,
        )
        self.workers.append(worker)
        try:
            worker.start()
        except RuntimeError as error:
            with self.active_lock:
                self.active.pop(request_id, None)
            raise failure("internal_error", str(error), request_id=request_id) from error

    def _cancel(self, params: Any) -> None:
        self.state.require_method("$/cancelRequest")
        params = validate_cancel_params(params)
        with self.active_lock:
            call = self.active.get(params["id"])
            if call is not None:
                call.reason = params.get("reason") or "request cancelled"
                call.cancelled.set()

    def _cancel_all(self, reason: str) -> None:
        with self.active_lock:
            for call in self.active.values():
                call.reason = reason
                call.cancelled.set()

    def _shutdown(self, params: Any, request_id: Any) -> None:
        self.state.begin_shutdown()
        validate_shutdown_params(params)
        self._cancel_all("shutdown")
        deadline = time.monotonic() + 1.0
        for worker in self.workers:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            worker.join(remaining)
        self.send(success_response(request_id, {"terminal": "shutdown"}))
        self.state.finish_shutdown()
        self.stop_event.set()

    def dispatch(self, request_id: Any, method: str, params: Any, notification: bool) -> None:
        if not self.state.initialized:
            if method != "initialize":
                raise failure("unknown_method", "only initialize is available before negotiation", request_id=request_id)
            result = self.state.initialize(params, [TOOL_DEFINITION])
            self.send(success_response(request_id, result))
            return
        self.state.require_method(method)
        if method == "initialize":
            raise failure("invalid_request", "initialize may only be called once", request_id=request_id)
        if method == "tool/call":
            if notification:
                raise failure("invalid_request", "tool/call requires an id", request_id=request_id)
            self._start_tool(params, request_id)
        elif method == "$/cancelRequest":
            if not notification:
                raise failure("invalid_request", "$/cancelRequest is a notification", request_id=request_id)
            self._cancel(params)
        elif method == "shutdown":
            if notification:
                raise failure("invalid_request", "shutdown requires an id", request_id=request_id)
            self._shutdown(params, request_id)
        else:
            raise failure("unknown_method", f"method {method!r} is not implemented", request_id=request_id)

    def _reply_or_diagnose(self, request_id: Any, notification: bool, error: ProtocolFailure) -> None:
        if not notification and request_id is not None:
            self.send_error(request_id, error)
        else:
            self.diagnostic(f"{error.name}: {error.detail}")

    def run(self) -> None:
        try:
            while not self.stop_event.is_set():
                try:
                    message = read_frame(self.stdin, self.state.max_frame_bytes)
                except ProtocolFailure as error:
                    self.diagnostic(f"{error.name}: {error.detail}")
                    break
                if message is None:
                    break
                request_id: Any = None
                method = ""
                notification = True
                try:
                    request_id, method, params, notification = parse_envelope(message)
                    self.dispatch(request_id, method, params, notification)
                except ProtocolFailure as error:
                    if error.request_id is not None:
                        request_id = error.request_id
                        notification = False
                    self._reply_or_diagnose(request_id, notification, error)
                    if method == "initialize" or not self.state.initialized:
                        self.stop_event.set()
                except Exception as error:  # pragma: no cover - defensive process boundary
                    self.diagnostic(f"internal error: {error}")
                    if not notification and request_id is not None:
                        self.send_error(request_id, failure("internal_error"))
                    if method == "initialize":
                        self.stop_event.set()
        finally:
            self._cancel_all("process stopped")
            deadline = time.monotonic() + 0.25
            for worker in self.workers:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                worker.join(remaining)


def main() -> int:
    expected_version = os.environ.get("OCTET_EXTENSION_API_VERSION")
    if expected_version is not None and expected_version != API_VERSION:
        print(
            f"octet-computer-use: version mismatch: host requested {expected_version!r}, expected {API_VERSION!r}",
            file=sys.stderr,
            flush=True,
        )
        return 2
    ComputerUseExtension(sys.stdin.buffer, sys.stdout.buffer).run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
