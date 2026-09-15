"""Trusted-local macOS composition; no wire service or native qualification.

The embedding selects the owner, exact native identity, scope and factory. The
standalone API 0.3 process supplies none of these and remains inert. This adapter
never executes model code and never treats native confirmation as host policy.
"""
from __future__ import annotations

import hashlib
import json
import threading
from dataclasses import replace
from typing import Any, Mapping

from .lifecycle import (
    ActionSpec, AdapterActionError, CancellationToken, LifecycleError,
    LifecycleSession, OwnerIdentity, PolicyDecision as LifecycleDecision,
    TargetIdentity, TargetObservation, VerificationResult, bounded_call,
    _BoundedCallInterrupted,
)
from .policy import Decision, PolicyDenied, PolicyGate, make_action_request
from .macos.model import MacOSBackendError


def _wire(value: Any) -> Any:
    # API 0.3 forbids floats. Preserve fractional inspection data as decimal
    # text, never silently round an input coordinate or an authority binding.
    if isinstance(value, float):
        return int(value) if value.is_integer() else repr(value)
    if isinstance(value, dict):
        return {key: _wire(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [_wire(item) for item in value]
    return value


def _digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     ensure_ascii=False, allow_nan=False).encode()).hexdigest()


class MacOSRuntime:
    """One selected owner/target; serial calls and terminal trusted stop.

    ``backend_factory`` is required: there is deliberately no implicit native
    launch/permission request. It must return an owner-local MacOSBackend. Only
    the overlap of the typed lifecycle and native backend contracts is admitted.
    """

    operations = frozenset({"observe", "click", "keypress", "type", "scroll"})

    def __init__(self, *, enabled: bool, owner: OwnerIdentity, native_target: Any,
                 policy_gate: PolicyGate, backend_factory: Any) -> None:
        from .macos.model import TargetIdentity as NativeTarget
        self.owner = OwnerIdentity.from_value(owner)
        self.native_target = NativeTarget.from_value(native_target)
        scope = policy_gate.scope
        if (enabled is not True or scope is None or not callable(backend_factory)
                or not self.native_target.process_start_token
                or scope.target.kind != "desktop"
                or scope.target.app_id != self.native_target.bundle_id
                or scope.target.window_id != str(self.native_target.window_id)
                or scope.session_id != self.owner.session_id
                or scope.owner_id != self.owner.extension_instance_id
                or scope.extension_generation != self.owner.process_generation):
            raise PolicyDenied("explicit matching owner, native target, scope and enablement required")
        self.gate = policy_gate
        self._factory = backend_factory
        self._backend = None
        self._lock = threading.Lock()
        self._evidence_lock = threading.Lock()
        self._stopped = threading.Event()
        self._raw = None
        self._digest = None
        self._frame = 0
        self._sequence = 0
        self._parent = None
        self._approval = None
        self._classes = ()
        self._grant = None
        self._target = TargetIdentity(scope.target.app_id, scope.target.window_id)
        self.session = LifecycleSession(self.owner, self._target, self, policy=self)

    def _check(self, owner: OwnerIdentity, cancellation: Any = None) -> None:
        if owner != self.owner or self._stopped.is_set():
            raise LifecycleError("owner_settled", "The runtime owner is not active.")
        if cancellation is not None:
            cancellation.throw_if_cancelled()

    def _backend_instance(self) -> Any:
        self._check(self.owner)
        if self._backend is None:
            self._backend = self._factory()
            if self._stopped.is_set():
                self._backend.stop(owner=self.owner.as_dict())
                self._check(self.owner)
        return self._backend

    def _request(self, operation: str, arguments: Mapping[str, Any], observation: Any = None) -> Any:
        scope = self.gate.scope
        return make_action_request(
            operation=operation, arguments=dict(arguments), target=scope.target,
            session_id=self.owner.session_id, owner_id=self.owner.extension_instance_id,
            extension_generation=self.owner.process_generation, scope=scope.scope_id,
            frame_generation=observation.target.frame_generation if observation else max(1, self._frame),
            observation_digest=observation.digest if observation else None,
            native_identity_digest=_digest(self.native_target.as_dict()),
            trusted_data_classes=self._classes,
        )

    def call(self, operation: str, arguments: Mapping[str, Any], context: Mapping[str, Any],
             cancellation: threading.Event) -> Mapping[str, Any]:
        if operation not in self.operations:
            raise PolicyDenied("operation is not supported by the bounded macOS composition")
        if not self._lock.acquire(blocking=False):
            raise PolicyDenied("computer-use runtime is busy")
        try:
            self._check(self.owner)
            scope = self.gate.scope
            if (context["session_id"] != self.owner.session_id
                    or context["owner_id"] != self.owner.extension_instance_id
                    or context["extension_generation"] != self.owner.process_generation
                    or context["target"] != scope.target or context["scope"] != scope.scope_id
                    or context.get("destination") is not None):
                raise PolicyDenied("host context does not match the selected runtime")
            self._parent = context["parent_request_id"]
            self._approval = context.get("approval_token")
            self._classes = context.get("trusted_data_classes", ())
            token = _EventToken(cancellation)
            token.throw_if_cancelled()
            if operation == "observe":
                if arguments:
                    raise PolicyDenied("observe accepts no arguments")
                request = self._request(operation, arguments)
                try:
                    decision = bounded_call(
                        lambda: self.gate.evaluate(request, parent_request_id=self._parent,
                                                   approval_token=self._approval),
                        token=token, timeout=self.session.budget.budget.max_action_seconds,
                        operation="observation policy",
                    )
                except _BoundedCallInterrupted as error:
                    raise LifecycleError(error.reason, "Observation policy was interrupted.") from error
                token.throw_if_cancelled()
                self.gate.authorize(request, decision, parent_request_id=self._parent)
                observation = self.session.observe(self.owner, cancellation=token)
                self._check(self.owner, token)
                return {"status": "completed", "operation": operation, "result": _wire(observation.as_dict())}
            prior = self.session.last_observation
            if prior is None or context["frame_generation"] != prior.target.frame_generation:
                raise PolicyDenied("a matching current observation is required")
            if operation == "click":
                coordinate_keys = set(arguments) & {"x", "y", "coordinates"}
                if (coordinate_keys not in ({"x", "y"}, {"coordinates"})
                        or arguments.get("button", "left") != "left"):
                    raise PolicyDenied("click requires one exact coordinate pair and the left button")
            action = ActionSpec("press" if operation == "keypress" else operation,
                                prior.target, prior.observation_id, arguments,
                                expected_digest=prior.digest)
            result = self.session.execute_group(self.owner, (action,), cancellation=token)
            if cancellation.is_set() or result.status not in {"succeeded", "denied", "stale_observation"}:
                self.stop("action_not_completed")
            # Acknowledged input is not verified task success.
            return {"status": "completed" if result.ok else "denied", "operation": operation,
                    "result": _wire(result.as_dict())}
        except (LifecycleError, PolicyDenied) as error:
            if isinstance(error, LifecycleError) or cancellation.is_set():
                self.stop("cancelled" if cancellation.is_set() else "lifecycle_failed")
            raise
        finally:
            self._grant = None
            self._parent = None
            self._lock.release()

    def observe(self, owner: OwnerIdentity, target: TargetIdentity, *,
                cancellation: CancellationToken, timeout: float) -> TargetObservation:
        self._check(owner, cancellation)
        try:
            self.gate.check_scope(self._request("observe", {}))
        except PolicyDenied as error:
            raise LifecycleError("scope_unavailable", str(error)) from error
        try:
            raw = self._backend_instance().observe(self.native_target, owner=self.owner.as_dict())
        except MacOSBackendError as error:
            raise LifecycleError("target_changed", "Native observation is no longer available.") from error
        self._check(owner, cancellation)
        if raw["target"] != self.native_target.as_dict():
            raise LifecycleError("target_changed", "Native identity changed.")
        evidence = {key: raw[key] for key in ("target", "window", "accessibility")}
        digest = _digest(evidence)
        with self._evidence_lock:
            self._check(owner, cancellation)
            if digest != self._digest:
                self._frame += 1
            self._sequence += 1
            self._raw, self._digest = raw, digest
        geometry = raw["window"]["geometry"]
        if any(float(geometry[key]) != int(geometry[key]) for key in ("width", "height")):
            raise LifecycleError("unsupported_geometry", "Fractional window dimensions are not supported by this composition.")
        return TargetObservation(owner, replace(self._target, frame_generation=self._frame),
            "native-" + str(self._sequence), self._sequence,
            int(geometry["width"]), int(geometry["height"]), 1.0,
            (1, 0, 0, 1, 0, 0), digest,
            structured={"accessibility": raw["accessibility"], "geometry": raw["window"]["geometry"]})

    def decide(self, owner: OwnerIdentity, action: ActionSpec, observation: TargetObservation,
               **_: Any) -> LifecycleDecision:
        self._check(owner)
        operation = "keypress" if action.kind == "press" else action.kind
        request = self._request(operation, action.parameters, observation)
        decision = self.gate.evaluate(request, parent_request_id=self._parent,
                                      approval_token=self._approval)
        if decision.decision != Decision.ALLOW:
            return LifecycleDecision.deny(decision.reason)
        with self._evidence_lock:
            self._check(owner)
            self._grant = (action, request, decision, observation)
        return LifecycleDecision.allow()

    def perform_action(self, owner: OwnerIdentity, target: TargetIdentity, action: ActionSpec, *,
                       cancellation: CancellationToken, timeout: float) -> Mapping[str, Any]:
        self._check(owner, cancellation)
        if self._grant is None or self._grant[0] != action:
            raise AdapterActionError("policy_denied", "No exact action grant.", may_have_effect=False)
        _, request, decision, approved = self._grant
        self._grant = None
        fresh = self.observe(owner, target, cancellation=cancellation, timeout=timeout)
        if fresh.digest != approved.digest or fresh.target != approved.target:
            raise AdapterActionError("target_changed", "Target changed during policy.", may_have_effect=False)
        native = {"operation": action.kind, "target": self.native_target,
                  "observation": self._raw, **action.parameters}
        if action.kind == "press":
            native["key"] = {"ArrowUp": "Up", "ArrowDown": "Down",
                             "ArrowLeft": "Left", "ArrowRight": "Right"}.get(
                                 action.parameters["key"], action.parameters["key"])
        if action.kind in {"click", "type"}:
            nodes = self._raw["accessibility"]["nodes"]
            if action.kind == "click":
                point = action.parameters.get("coordinates", action.parameters)
                nodes = [node for node in nodes if "bounds" in node
                         and point.get("x") == node["bounds"]["x"] + node["bounds"]["width"] / 2
                         and point.get("y") == node["bounds"]["y"] + node["bounds"]["height"] / 2]
                if action.parameters.get("button", "left") != "left":
                    nodes = []
            else:
                native["operation"] = "type_text"
                nodes = [node for node in nodes if node["focused"] and node["editable"]]
            if len(nodes) != 1:
                raise AdapterActionError("target_ambiguous", "One exact observed AX control is required.", may_have_effect=False)
            native["ref"] = nodes[0]["ref"]
        def authorize() -> None:
            # The native backend invokes this only after its own final identity,
            # permission and AX checks, immediately before input dispatch.
            self._check(owner, cancellation)
            self.gate.authorize(request, decision, parent_request_id=self._parent)

        self._check(owner, cancellation)
        self._backend_instance().perform(native, owner=owner.as_dict(),
            confirmation=True, cancellation=cancellation, authorization=authorize)
        return {"outcome": "committed", "acknowledged": True,
                "detail": "Native input acknowledged; task success is not inferred."}

    def verify(self, owner: OwnerIdentity, target: TargetIdentity, **_: Any) -> VerificationResult:
        self._check(owner)
        return VerificationResult.from_observation(self.session.last_observation, trusted=False,
            detail="Native target reobserved; task success has not been verified.")

    def release_input(self, owner: OwnerIdentity, **_: Any) -> bool:
        if owner != self.owner:
            return False
        if self._backend is None:
            return True
        return self._backend.stop(owner=owner.as_dict())["input_released"] is True

    def stop(self, reason: str = "host_stop") -> Mapping[str, Any]:
        self._stopped.set()
        self.gate.revoke()
        with self._evidence_lock:
            self._raw = None
            self._grant = None
        return self.session.settle(self.owner, reason=reason)

    def takeover(self) -> Mapping[str, Any]:
        return self.stop("host_takeover")


class _EventToken(CancellationToken):
    def __init__(self, event: threading.Event) -> None:
        super().__init__()
        self.event = event

    @property
    def cancelled(self) -> bool:
        return self.event.is_set() or super().cancelled
