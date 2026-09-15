"""Contained model-code transport for computer-use lifecycles.

This module is deliberately separate from :mod:`lifecycle`.  The lifecycle is
safe to import in a host process and contains no process or operating-system
sandboxing.  This module adds a *fail-closed* model-code transport:

* source is bounded and checked with a restrictive AST validator;
* model code has only ``observe``, ``act``, ``act_group``, and bounded output;
* computer effects are requests over a small JSON IPC bridge owned by the
  parent lifecycle;
* an ordinary child process is never treated as a sandbox; and
* code is run only after a genuine operating-system containment setup has
  succeeded.

The OS setup is intentionally conservative.  On systems where the required
Linux namespaces, resource limits, and no-new-privileges control are not
available, execution returns ``sandbox_unavailable`` or
``sandbox_setup_failed`` instead of falling back to an ordinary subprocess.
"""

from __future__ import annotations

import ast
import ctypes
import inspect
import json
import math
import multiprocessing
import os
import platform
import resource
import sys
import tempfile
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple

from .lifecycle import (
    CancellationToken,
    LifecycleError,
    OwnerIdentity,
    TargetIdentity,
    bounded_text,
)


MAX_SOURCE_BYTES = 64 * 1024
MAX_IPC_BYTES = 256 * 1024
MAX_CODE_OUTPUT_CHARS = 64 * 1024
MAX_BRIDGE_CALLS = 32
MAX_AST_NODES = 4096
MAX_AST_DEPTH = 32
MAX_LITERAL_CHARS = 16 * 1024
MAX_LITERAL_ITEMS = 128
MAX_LOOP_ITERATIONS = 1024
MAX_MEMORY_BYTES = 512 * 1024 * 1024
MAX_WALL_SECONDS = 60.0
MAX_CPU_SECONDS = 30.0

_SANDBOX_STATUSES = frozenset(
    {
        "succeeded",
        "cancelled",
        "budget_exhausted",
        "unknown_effect",
        "sandbox_setup_failed",
        "sandbox_unavailable",
        "rejected",
        "failed",
    }
)


class CodeRuntimeError(RuntimeError):
    """Base class for bounded code-runtime errors."""

    def __init__(self, code: str, message: str) -> None:
        self.code = bounded_text(code, 96)
        self.message = bounded_text(message, 2048)
        super().__init__(self.message)

    def as_dict(self) -> Dict[str, Any]:
        return {"code": self.code, "message": self.message}


class CodeValidationError(CodeRuntimeError):
    """Source was not in the deliberately small model-code language."""


class SandboxUnavailable(CodeRuntimeError):
    """The host cannot provide the required genuine OS containment."""


class _ChildBudgetError(Exception):
    pass


class _ChildBridgeError(Exception):
    def __init__(self, message: str, *, unknown_effect: bool = False) -> None:
        self.message = bounded_text(message, 2048)
        self.unknown_effect = bool(unknown_effect)
        super().__init__(self.message)


# ---------------------------------------------------------------------------
# Bounded values and public transport records


def _json_value(
    value: Any,
    *,
    depth: int = 6,
    items: int = MAX_LITERAL_ITEMS,
    text: int = MAX_LITERAL_CHARS,
) -> Any:
    """Copy a JSON value while rejecting host objects and non-finite numbers."""

    if depth < 0:
        raise CodeValidationError("value_too_deep", "A model-code value exceeded the nesting limit.")
    if value is None or isinstance(value, bool) or isinstance(value, int):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise CodeValidationError("non_finite_value", "Non-finite numbers are not allowed in model-code IPC.")
        return value
    if isinstance(value, str):
        if len(value.encode("utf-8", errors="replace")) > text:
            raise CodeValidationError("value_too_large", "A model-code string exceeded its bound.")
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        raise CodeValidationError("binary_value", "Binary values are not available to model code.")
    if isinstance(value, Mapping):
        result: Dict[str, Any] = {}
        for index, (key, item) in enumerate(value.items()):
            if index >= items:
                raise CodeValidationError("value_too_large", "A model-code object exceeded its item bound.")
            if not isinstance(key, str) or key.startswith("__"):
                raise CodeValidationError("invalid_key", "Model-code object keys must be bounded text.")
            result[key] = _json_value(item, depth=depth - 1, items=items, text=text)
        return result
    if isinstance(value, (list, tuple)):
        if len(value) > items:
            raise CodeValidationError("value_too_large", "A model-code array exceeded its item bound.")
        return [_json_value(item, depth=depth - 1, items=items, text=text) for item in value]
    raise CodeValidationError("host_object", "Host objects cannot cross the model-code boundary.")


def _json_bytes(value: Any, *, maximum: int = MAX_IPC_BYTES) -> bytes:
    copied = _json_value(value, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_IPC_BYTES)
    try:
        encoded = json.dumps(copied, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise CodeValidationError("invalid_json", "The model-code IPC value is not JSON serializable.") from error
    if len(encoded) > maximum:
        raise CodeValidationError("ipc_limit_exceeded", "The model-code IPC message exceeded its bound.")
    return encoded


def _error_mapping(error: Any, default_code: str = "code_runtime_failed") -> Dict[str, str]:
    if isinstance(error, CodeRuntimeError):
        return error.as_dict()
    if isinstance(error, LifecycleError):
        return {
            "code": bounded_text(error.code, 96),
            "message": bounded_text(error.message, 2048),
        }
    if isinstance(error, Mapping):
        code = error.get("code", default_code)
        message = error.get("message", "The bounded model-code operation failed.")
        return {"code": bounded_text(code, 96), "message": bounded_text(message, 2048)}
    return {"code": default_code, "message": "The bounded model-code operation failed safely."}


def _result_mapping(value: Any) -> Dict[str, Any]:
    if isinstance(value, Mapping):
        return dict(_json_value(value, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_IPC_BYTES))
    as_dict = getattr(value, "as_dict", None)
    if callable(as_dict):
        result = as_dict()
        if isinstance(result, Mapping):
            return dict(_json_value(result, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_IPC_BYTES))
    raise CodeRuntimeError("invalid_bridge_result", "A parent bridge operation returned a non-JSON result.")


@dataclass(frozen=True)
class CodeBudget:
    """Finite limits for one model-code invocation."""

    max_wall_seconds: float = 10.0
    max_cpu_seconds: float = 5.0
    max_memory_bytes: int = 128 * 1024 * 1024
    max_output_chars: int = MAX_CODE_OUTPUT_CHARS
    max_ipc_bytes: int = MAX_IPC_BYTES
    max_bridge_calls: int = MAX_BRIDGE_CALLS
    max_loop_iterations: int = MAX_LOOP_ITERATIONS

    def __post_init__(self) -> None:
        for name in (
            "max_wall_seconds",
            "max_cpu_seconds",
        ):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(float(value)) or value <= 0:
                raise ValueError(name + " must be finite and positive")
            if float(value) > (MAX_WALL_SECONDS if name == "max_wall_seconds" else MAX_CPU_SECONDS):
                raise ValueError(name + " exceeds the code-runtime bound")
        for name, maximum in (
            ("max_memory_bytes", MAX_MEMORY_BYTES),
            ("max_output_chars", MAX_CODE_OUTPUT_CHARS),
            ("max_ipc_bytes", MAX_IPC_BYTES),
            ("max_bridge_calls", MAX_BRIDGE_CALLS),
            ("max_loop_iterations", MAX_LOOP_ITERATIONS),
        ):
            value = getattr(self, name)
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0 or value > maximum:
                raise ValueError(name + " exceeds the code-runtime bound")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "max_wall_seconds": self.max_wall_seconds,
            "max_cpu_seconds": self.max_cpu_seconds,
            "max_memory_bytes": self.max_memory_bytes,
            "max_output_chars": self.max_output_chars,
            "max_ipc_bytes": self.max_ipc_bytes,
            "max_bridge_calls": self.max_bridge_calls,
            "max_loop_iterations": self.max_loop_iterations,
        }


@dataclass(frozen=True)
class SandboxRestrictions:
    """Capabilities presented to model code; all host capabilities are denied."""

    allow_imports: bool = False
    allow_filesystem: bool = False
    allow_network: bool = False
    allow_subprocess: bool = False
    allow_host_runtime: bool = False
    namespace_isolation: bool = True
    resource_limits: bool = True
    no_new_privileges: bool = True

    @property
    def deny_by_default(self) -> bool:
        return not (
            self.allow_imports
            or self.allow_filesystem
            or self.allow_network
            or self.allow_subprocess
            or self.allow_host_runtime
        )

    def as_dict(self) -> Dict[str, Any]:
        return {
            "allow_imports": self.allow_imports,
            "allow_filesystem": self.allow_filesystem,
            "allow_network": self.allow_network,
            "allow_subprocess": self.allow_subprocess,
            "allow_host_runtime": self.allow_host_runtime,
            "namespace_isolation": self.namespace_isolation,
            "resource_limits": self.resource_limits,
            "no_new_privileges": self.no_new_privileges,
        }


@dataclass(frozen=True)
class SandboxCapabilities:
    """Measured/qualified OS containment capabilities.

    ``genuine`` is normalized in ``__post_init__``: a caller cannot claim a
    genuine sandbox while omitting any required isolation dimension.
    """

    genuine: bool = False
    os_isolated: bool = False
    network_isolated: bool = False
    filesystem_isolated: bool = False
    process_isolated: bool = False
    resource_limits: bool = False
    no_new_privileges: bool = False
    platform: str = ""
    detail: str = ""

    def __post_init__(self) -> None:
        required = (
            self.os_isolated,
            self.network_isolated,
            self.filesystem_isolated,
            self.process_isolated,
            self.resource_limits,
            self.no_new_privileges,
        )
        object.__setattr__(self, "genuine", bool(self.genuine and all(required)))
        object.__setattr__(self, "platform", bounded_text(self.platform, 64))
        object.__setattr__(self, "detail", bounded_text(self.detail, 1024))

    @property
    def available(self) -> bool:
        return self.genuine

    # Read-compatible aliases used by host qualification documents.
    @property
    def os_isolation(self) -> bool:
        return self.os_isolated

    @property
    def network_isolation(self) -> bool:
        return self.network_isolated

    @property
    def filesystem_isolation(self) -> bool:
        return self.filesystem_isolated

    @property
    def subprocess_isolation(self) -> bool:
        return self.process_isolated

    def as_dict(self) -> Dict[str, Any]:
        return {
            "genuine": self.genuine,
            "available": self.available,
            "os_isolated": self.os_isolated,
            "network_isolated": self.network_isolated,
            "filesystem_isolated": self.filesystem_isolated,
            "process_isolated": self.process_isolated,
            "resource_limits": self.resource_limits,
            "no_new_privileges": self.no_new_privileges,
            "platform": self.platform,
            "detail": self.detail,
        }


@dataclass(frozen=True)
class SandboxRequest:
    source: str
    budget: CodeBudget = field(default_factory=CodeBudget)
    restrictions: SandboxRestrictions = field(default_factory=SandboxRestrictions)
    metadata: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not isinstance(self.source, str) or not self.source.strip():
            raise CodeValidationError("empty_source", "Model-code source must be non-empty text.")
        if len(self.source.encode("utf-8", errors="replace")) > MAX_SOURCE_BYTES:
            raise CodeValidationError("source_too_large", "Model-code source exceeded its bound.")
        if not isinstance(self.budget, CodeBudget):
            object.__setattr__(self, "budget", CodeBudget(**dict(self.budget)))
        if not isinstance(self.restrictions, SandboxRestrictions):
            object.__setattr__(self, "restrictions", SandboxRestrictions(**dict(self.restrictions)))
        copied = _json_value(self.metadata, depth=3, items=32, text=1024)
        if not isinstance(copied, Mapping):
            raise CodeValidationError("invalid_metadata", "Sandbox metadata must be a JSON object.")
        object.__setattr__(self, "metadata", dict(copied))

    @property
    def code(self) -> str:
        return self.source

    def as_dict(self) -> Dict[str, Any]:
        return {
            "source": self.source,
            "budget": self.budget.as_dict(),
            "restrictions": self.restrictions.as_dict(),
            "metadata": dict(self.metadata),
        }


@dataclass(frozen=True)
class SandboxResult:
    status: str
    output: str = ""
    value: Any = None
    error: Optional[Mapping[str, Any]] = None
    bridge_calls: int = 0
    unknown_effects: Tuple[Mapping[str, Any], ...] = ()
    capabilities: Optional[SandboxCapabilities] = None
    usage: Optional[Mapping[str, Any]] = None

    def __post_init__(self) -> None:
        status = self.status if self.status in _SANDBOX_STATUSES else "failed"
        object.__setattr__(self, "status", status)
        object.__setattr__(self, "output", bounded_text(self.output, MAX_CODE_OUTPUT_CHARS))
        if self.error is not None:
            object.__setattr__(self, "error", _error_mapping(self.error))
        if not isinstance(self.bridge_calls, int) or isinstance(self.bridge_calls, bool) or self.bridge_calls < 0:
            object.__setattr__(self, "bridge_calls", 0)
        try:
            safe_value = _json_value(self.value, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_CODE_OUTPUT_CHARS)
        except CodeRuntimeError:
            safe_value = None
        object.__setattr__(self, "value", safe_value)
        safe_unknown: List[Mapping[str, Any]] = []
        for item in self.unknown_effects:
            try:
                copied = _json_value(item, depth=5, items=32, text=MAX_CODE_OUTPUT_CHARS)
            except CodeRuntimeError:
                continue
            if isinstance(copied, Mapping):
                safe_unknown.append(dict(copied))
        object.__setattr__(self, "unknown_effects", tuple(safe_unknown[:MAX_BRIDGE_CALLS]))
        if self.usage is not None:
            try:
                copied_usage = _json_value(self.usage, depth=4, items=32, text=1024)
            except CodeRuntimeError:
                copied_usage = {}
            object.__setattr__(self, "usage", copied_usage if isinstance(copied_usage, Mapping) else {})

    @property
    def ok(self) -> bool:
        return self.status == "succeeded" and not self.unknown_effects

    def as_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "status": self.status,
            "ok": self.ok,
            "output": self.output,
            "value": self.value,
            "bridge_calls": self.bridge_calls,
            "unknown_effects": [dict(item) for item in self.unknown_effects],
        }
        if self.error is not None:
            result["error"] = dict(self.error)
        if self.capabilities is not None:
            result["capabilities"] = self.capabilities.as_dict()
        if self.usage is not None:
            result["usage"] = dict(self.usage)
        return result


@dataclass(frozen=True)
class CodeExecutionResult:
    status: str
    output: str = ""
    value: Any = None
    error: Optional[Mapping[str, Any]] = None
    bridge_calls: int = 0
    unknown_effects: Tuple[Mapping[str, Any], ...] = ()
    sandbox: Optional[SandboxResult] = None

    def __post_init__(self) -> None:
        status = self.status if self.status in _SANDBOX_STATUSES else "failed"
        object.__setattr__(self, "status", status)
        object.__setattr__(self, "output", bounded_text(self.output, MAX_CODE_OUTPUT_CHARS))
        if self.error is not None:
            object.__setattr__(self, "error", _error_mapping(self.error))
        try:
            object.__setattr__(self, "value", _json_value(self.value, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_CODE_OUTPUT_CHARS))
        except CodeRuntimeError:
            object.__setattr__(self, "value", None)
        if not isinstance(self.bridge_calls, int) or isinstance(self.bridge_calls, bool) or self.bridge_calls < 0:
            object.__setattr__(self, "bridge_calls", 0)
        copied_unknown: List[Mapping[str, Any]] = []
        for item in self.unknown_effects:
            try:
                safe_item = _json_value(item, depth=5, items=32, text=MAX_CODE_OUTPUT_CHARS)
            except CodeRuntimeError:
                continue
            if isinstance(safe_item, Mapping):
                copied_unknown.append(dict(safe_item))
        object.__setattr__(self, "unknown_effects", tuple(copied_unknown[:MAX_BRIDGE_CALLS]))

    @property
    def ok(self) -> bool:
        return self.status == "succeeded" and not self.unknown_effects

    def as_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "status": self.status,
            "ok": self.ok,
            "output": self.output,
            "value": self.value,
            "bridge_calls": self.bridge_calls,
            "unknown_effects": [dict(item) for item in self.unknown_effects],
        }
        if self.error is not None:
            result["error"] = dict(self.error)
        if self.sandbox is not None:
            result["sandbox"] = self.sandbox.as_dict()
        return result


@dataclass(frozen=True)
class CodeExecutionContext:
    """The only operations made available to a model-code bridge.

    This object is a host-side convenience for integrations and is not used as
    the child process namespace.  The child namespace contains only the same
    four named operations, so it never receives a host object or callable.
    """

    observe: Callable[..., Any]
    act: Callable[..., Any]
    act_group: Callable[..., Any]
    emit: Callable[..., Any]

    def __post_init__(self) -> None:
        for function in (self.observe, self.act, self.act_group, self.emit):
            if not callable(function):
                raise TypeError("code execution context operations must be callable")

    def as_namespace(self) -> Dict[str, Callable[..., Any]]:
        return {
            "observe": self.observe,
            "act": self.act,
            "act_group": self.act_group,
            "emit": self.emit,
        }


# ---------------------------------------------------------------------------
# AST validation


_SAFE_CALLS = frozenset(
    {
        "observe",
        "act",
        "act_group",
        "emit",
        "print",
        "len",
        "range",
        "enumerate",
        "min",
        "max",
        "sum",
        "abs",
        "bool",
        "int",
        "float",
        "str",
        "list",
        "dict",
        "tuple",
    }
)
_SAFE_NAMES = _SAFE_CALLS | {"True", "False", "None"}

_ALLOWED_AST_NODES = (
    ast.Module,
    ast.Expr,
    ast.Assign,
    ast.AnnAssign,
    ast.AugAssign,
    ast.If,
    ast.For,
    ast.Break,
    ast.Continue,
    ast.Pass,
    ast.Name,
    ast.Load,
    ast.Store,
    ast.Del,
    ast.Constant,
    ast.List,
    ast.Tuple,
    ast.Dict,
    ast.Set,
    ast.Subscript,
    ast.Slice,
    ast.BinOp,
    ast.UnaryOp,
    ast.BoolOp,
    ast.Compare,
    ast.IfExp,
    ast.Call,
    ast.keyword,
    ast.Add,
    ast.Sub,
    ast.Mult,
    ast.Div,
    ast.FloorDiv,
    ast.Mod,
    ast.Pow,
    ast.UAdd,
    ast.USub,
    ast.Not,
    ast.Invert,
    ast.And,
    ast.Or,
    ast.Eq,
    ast.NotEq,
    ast.Lt,
    ast.LtE,
    ast.Gt,
    ast.GtE,
    ast.In,
    ast.NotIn,
)


class _SourceValidator(ast.NodeVisitor):
    def __init__(self, *, max_nodes: int = MAX_AST_NODES, max_depth: int = MAX_AST_DEPTH) -> None:
        self.max_nodes = max_nodes
        self.max_depth = max_depth
        self.nodes = 0
        self.depth = 0
        self.assigned: set[str] = set()

    def fail(self, code: str, message: str, node: Optional[ast.AST] = None) -> None:
        suffix = ""
        if node is not None and getattr(node, "lineno", None) is not None:
            suffix = " at line " + str(node.lineno)
        raise CodeValidationError(code, message + suffix)

    def visit(self, node: ast.AST) -> Any:  # type: ignore[override]
        self.nodes += 1
        if self.nodes > self.max_nodes:
            self.fail("ast_too_large", "Model-code AST exceeded its node bound.", node)
        if not isinstance(node, _ALLOWED_AST_NODES):
            self.fail("ast_node_denied", "The model-code AST contains a denied construct.", node)
        self.depth += 1
        if self.depth > self.max_depth:
            self.fail("ast_too_deep", "Model-code AST exceeded its nesting bound.", node)
        try:
            return super().visit(node)
        finally:
            self.depth -= 1

    def visit_Name(self, node: ast.Name) -> Any:
        if not node.id or node.id.startswith("_"):
            self.fail("name_denied", "Private and dunder names are unavailable to model code.", node)
        if isinstance(node.ctx, ast.Load) and node.id not in _SAFE_NAMES and node.id not in self.assigned:
            self.fail("name_denied", "Model code referenced a name outside its allowlist.", node)
        if isinstance(node.ctx, ast.Store):
            self.assigned.add(node.id)

    def _assignment_target(self, node: ast.AST) -> None:
        if isinstance(node, ast.Name):
            self.visit(node)
            return
        if isinstance(node, (ast.Tuple, ast.List)):
            for item in node.elts:
                self._assignment_target(item)
            return
        self.fail("assignment_denied", "Model code may assign only local names.", node)

    def visit_Assign(self, node: ast.Assign) -> Any:
        if len(node.targets) > MAX_LITERAL_ITEMS:
            self.fail("ast_too_large", "Too many assignment targets.", node)
        for target in node.targets:
            self._assignment_target(target)
        self.visit(node.value)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> Any:
        self._assignment_target(node.target)
        if node.annotation is not None:
            self.fail("annotation_denied", "Type annotations are not part of model code.", node)
        if node.value is not None:
            self.visit(node.value)

    def visit_AugAssign(self, node: ast.AugAssign) -> Any:
        if not isinstance(node.target, ast.Name):
            self.fail("assignment_denied", "Model code may update only local names.", node)
        self.visit(node.target)
        self.visit(node.value)
        self.visit(node.op)

    def visit_For(self, node: ast.For) -> Any:
        self._assignment_target(node.target)
        self.visit(node.iter)
        for statement in node.body:
            self.visit(statement)
        for statement in node.orelse:
            self.visit(statement)

    def visit_Call(self, node: ast.Call) -> Any:
        if not isinstance(node.func, ast.Name) or node.func.id not in _SAFE_CALLS:
            self.fail("call_denied", "Only bounded model-code and bridge calls are available.", node)
        if len(node.args) > 16 or len(node.keywords) > 16:
            self.fail("call_too_large", "Model-code calls have a bounded argument count.", node)
        for keyword in node.keywords:
            if keyword.arg is None:
                self.fail("call_denied", "Keyword expansion is unavailable to model code.", node)
            if keyword.arg.startswith("_"):
                self.fail("call_denied", "Private keyword arguments are unavailable.", node)
            self.visit(keyword.value)
        for argument in node.args:
            if isinstance(argument, ast.Starred):
                self.fail("call_denied", "Argument expansion is unavailable to model code.", node)
            self.visit(argument)

    def visit_Constant(self, node: ast.Constant) -> Any:
        value = node.value
        if isinstance(value, (bytes, bytearray, complex)):
            self.fail("literal_denied", "Binary and complex literals are unavailable.", node)
        if isinstance(value, str) and len(value.encode("utf-8", errors="replace")) > MAX_LITERAL_CHARS:
            self.fail("literal_too_large", "A model-code literal exceeded its bound.", node)
        if isinstance(value, int) and abs(value) > 2**53:
            self.fail("literal_too_large", "Integer literals are bounded to JSON-safe precision.", node)
        if isinstance(value, float) and not math.isfinite(value):
            self.fail("literal_denied", "Non-finite literals are unavailable.", node)

    def visit_List(self, node: ast.List) -> Any:
        if len(node.elts) > MAX_LITERAL_ITEMS:
            self.fail("literal_too_large", "A model-code list exceeded its item bound.", node)
        for item in node.elts:
            self.visit(item)

    visit_Tuple = visit_List
    visit_Set = visit_List

    def visit_Dict(self, node: ast.Dict) -> Any:
        if len(node.keys) > MAX_LITERAL_ITEMS:
            self.fail("literal_too_large", "A model-code object exceeded its item bound.", node)
        for key, value in zip(node.keys, node.values):
            if key is None:
                self.fail("literal_denied", "Object expansion is unavailable to model code.", node)
            self.visit(key)
            self.visit(value)

    def visit_Subscript(self, node: ast.Subscript) -> Any:
        self.visit(node.value)
        self.visit(node.slice)

    def visit_Slice(self, node: ast.Slice) -> Any:
        for item in (node.lower, node.upper, node.step):
            if item is not None:
                self.visit(item)

    def visit_BinOp(self, node: ast.BinOp) -> Any:
        if not isinstance(node.op, (ast.Add, ast.Sub, ast.Mult, ast.Div, ast.FloorDiv, ast.Mod, ast.Pow)):
            self.fail("operator_denied", "The model-code operator is unavailable.", node)
        self.visit(node.left)
        self.visit(node.op)
        self.visit(node.right)

    def generic_visit(self, node: ast.AST) -> Any:
        return super().generic_visit(node)


def validate_source(source: str, *, restrictions: Optional[SandboxRestrictions] = None) -> ast.Module:
    """Parse and validate source without executing it."""

    if not isinstance(source, str):
        raise CodeValidationError("invalid_source", "Model-code source must be text.")
    if not source.strip():
        raise CodeValidationError("empty_source", "Model-code source must be non-empty.")
    if len(source.encode("utf-8", errors="replace")) > MAX_SOURCE_BYTES:
        raise CodeValidationError("source_too_large", "Model-code source exceeded its bound.")
    selected_restrictions = restrictions or SandboxRestrictions()
    if not selected_restrictions.deny_by_default:
        raise CodeValidationError("capability_denied", "Model-code capabilities are deny-by-default.")
    try:
        tree = ast.parse(source, mode="exec")
    except (SyntaxError, ValueError, TypeError) as error:
        raise CodeValidationError("invalid_syntax", "Model-code source is not valid bounded syntax.") from error
    validator = _SourceValidator()
    validator.visit(tree)
    return tree


# ---------------------------------------------------------------------------
# Child-side execution and JSON IPC


def _child_send(connection: Any, value: Mapping[str, Any], maximum: int) -> None:
    encoded = _json_bytes(value, maximum=maximum)
    connection.send_bytes(encoded)


def _child_recv(connection: Any, maximum: int) -> Mapping[str, Any]:
    try:
        raw = connection.recv_bytes(maximum)
    except (EOFError, OSError, ValueError) as error:
        raise _ChildBridgeError("The parent bridge closed or exceeded its IPC bound.") from error
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise _ChildBridgeError("The parent bridge returned invalid JSON.") from error
    if not isinstance(value, Mapping):
        raise _ChildBridgeError("The parent bridge returned a non-object JSON value.")
    return value


def _safe_child_range(maximum: int, *args: Any) -> range:
    try:
        result = range(*args)
        size = len(result)
    except (TypeError, ValueError, OverflowError) as error:
        raise _ChildBudgetError("range arguments are invalid or too large") from error
    if size > maximum:
        raise _ChildBudgetError("the model-code loop bound was exhausted")
    return result


def _safe_child_enumerate(maximum: int, value: Any, start: int = 0) -> Iterable[Tuple[int, Any]]:
    if not isinstance(start, int) or isinstance(start, bool):
        raise _ChildBudgetError("enumerate start must be an integer")
    count = 0
    for item in value:
        if count >= maximum:
            raise _ChildBudgetError("the model-code loop bound was exhausted")
        yield (start + count, item)
        count += 1


def _child_render(value: Any) -> str:
    try:
        safe = _json_value(value, depth=5, items=MAX_LITERAL_ITEMS, text=MAX_LITERAL_CHARS)
    except CodeRuntimeError:
        safe = "[unrepresentable]"
    if isinstance(safe, str):
        return safe
    try:
        return json.dumps(safe, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    except (TypeError, ValueError):
        return "[unrepresentable]"


def _child_execute(
    source: str,
    request: Mapping[str, Any],
    child_to_parent: Any,
    parent_to_child: Any,
) -> Mapping[str, Any]:
    budget_value = request.get("budget", {})
    budget = CodeBudget(**dict(budget_value)) if isinstance(budget_value, Mapping) else CodeBudget()
    restrictions_value = request.get("restrictions", {})
    restrictions = (
        SandboxRestrictions(**dict(restrictions_value))
        if isinstance(restrictions_value, Mapping)
        else SandboxRestrictions()
    )
    validate_source(source, restrictions=restrictions)
    output_parts: List[str] = []
    output_size = 0
    bridge_calls = 0
    bridge_sequence = 0
    unknown_effects: List[Mapping[str, Any]] = []

    def emit(value: Any = "", *, end: str = "\n", sep: str = " ") -> None:
        nonlocal output_size
        if not isinstance(end, str) or not isinstance(sep, str):
            raise _ChildBudgetError("output formatting is invalid")
        text = sep.join(_child_render(item) for item in value) if isinstance(value, tuple) else _child_render(value)
        text += end
        output_size += len(text)
        if output_size > budget.max_output_chars:
            raise _ChildBudgetError("model-code output budget exhausted")
        output_parts.append(text)

    def bridge(operation: str, arguments: Mapping[str, Any]) -> Any:
        nonlocal bridge_calls, bridge_sequence
        if bridge_calls >= budget.max_bridge_calls:
            raise _ChildBudgetError("model-code bridge-call budget exhausted")
        bridge_calls += 1
        bridge_sequence += 1
        safe_arguments = _json_value(arguments, depth=7, items=MAX_LITERAL_ITEMS, text=MAX_IPC_BYTES)
        if not isinstance(safe_arguments, Mapping):
            raise _ChildBridgeError("Bridge arguments must be a JSON object.")
        _child_send(
            child_to_parent,
            {
                "type": "bridge_request",
                "request_id": "bridge-" + str(bridge_sequence),
                "operation": operation,
                "arguments": dict(safe_arguments),
            },
            budget.max_ipc_bytes,
        )
        response = _child_recv(parent_to_child, budget.max_ipc_bytes)
        if response.get("type") != "bridge_result":
            raise _ChildBridgeError("The parent returned an invalid bridge response.")
        if not bool(response.get("ok", False)):
            unknown = bool(response.get("unknown_effect", False))
            if unknown:
                item = response.get("error", {"code": "unknown_effect"})
                if isinstance(item, Mapping):
                    unknown_effects.append(dict(_json_value(item, depth=4, items=32, text=MAX_LITERAL_CHARS)))
            error = response.get("error", {"code": "bridge_failed", "message": "The parent bridge rejected the operation."})
            message = error.get("message", "The parent bridge rejected the operation.") if isinstance(error, Mapping) else str(error)
            raise _ChildBridgeError(message, unknown_effect=unknown)
        value = response.get("result")
        return _json_value(value, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_IPC_BYTES)

    def observe(target: Any = None) -> Any:
        arguments: Dict[str, Any] = {}
        if target is not None:
            arguments["target"] = target
        return bridge("observe", arguments)

    def act(action: Any) -> Any:
        return bridge("act", {"action": action})

    def act_group(actions: Any) -> Any:
        return bridge("act_group", {"actions": actions})

    safe_builtins: Dict[str, Any] = {
        "bool": bool,
        "dict": dict,
        "float": float,
        "int": int,
        "len": len,
        "list": list,
        "max": max,
        "min": min,
        "range": lambda *args: _safe_child_range(budget.max_loop_iterations, *args),
        "enumerate": lambda value, start=0: _safe_child_enumerate(budget.max_loop_iterations, value, start),
        "str": str,
        "sum": sum,
        "tuple": tuple,
        "abs": abs,
        "print": emit,
    }
    namespace: Dict[str, Any] = {
        "__builtins__": safe_builtins,
        "observe": observe,
        "act": act,
        "act_group": act_group,
        "emit": emit,
    }
    try:
        exec(compile(source, "<model-code>", "exec"), namespace, namespace)
        value = namespace.get("result")
        safe_value = _json_value(value, depth=8, items=MAX_LITERAL_ITEMS, text=MAX_CODE_OUTPUT_CHARS)
        return {
            "type": "final",
            "status": "succeeded",
            "output": "".join(output_parts),
            "value": safe_value,
            "bridge_calls": bridge_calls,
            "unknown_effects": unknown_effects,
        }
    except _ChildBudgetError as error:
        return {
            "type": "final",
            "status": "budget_exhausted",
            "output": "".join(output_parts),
            "value": None,
            "bridge_calls": bridge_calls,
            "unknown_effects": unknown_effects,
            "error": {"code": "code_budget_exhausted", "message": bounded_text(str(error), 2048)},
        }
    except _ChildBridgeError as error:
        status = "unknown_effect" if error.unknown_effect else "failed"
        if error.unknown_effect:
            unknown_effects.append({"reason": "bridge_effect_unknown", "message": error.message})
        return {
            "type": "final",
            "status": status,
            "output": "".join(output_parts),
            "value": None,
            "bridge_calls": bridge_calls,
            "unknown_effects": unknown_effects,
            "error": {"code": "unknown_effect" if error.unknown_effect else "bridge_failed", "message": error.message},
        }
    except CodeRuntimeError as error:
        return {
            "type": "final",
            "status": "failed",
            "output": "".join(output_parts),
            "value": None,
            "bridge_calls": bridge_calls,
            "unknown_effects": unknown_effects,
            "error": error.as_dict(),
        }
    except BaseException:
        # Tracebacks and exception text can contain model-controlled or host
        # details.  Never return them across the IPC boundary.
        return {
            "type": "final",
            "status": "failed",
            "output": "".join(output_parts),
            "value": None,
            "bridge_calls": bridge_calls,
            "unknown_effects": unknown_effects,
            "error": {"code": "code_failed", "message": "Model-code execution failed safely."},
        }


# ---------------------------------------------------------------------------
# Linux containment


def _prctl_no_new_privileges() -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    prctl = getattr(libc, "prctl", None)
    if prctl is None:
        raise OSError("prctl is unavailable")
    prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    prctl.restype = ctypes.c_int
    # Linux PR_SET_NO_NEW_PRIVS.
    if prctl(38, 1, 0, 0, 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, "PR_SET_NO_NEW_PRIVS failed")


def _write_user_namespace_maps() -> None:
    uid = os.getuid()
    gid = os.getgid()
    try:
        with open("/proc/self/setgroups", "w", encoding="ascii") as stream:
            stream.write("deny\n")
    except OSError:
        # Kernels without setgroups control may still permit uid/gid maps; the
        # map write below is the authoritative check.
        pass
    with open("/proc/self/uid_map", "w", encoding="ascii") as stream:
        stream.write("0 %d 1\n" % uid)
    with open("/proc/self/gid_map", "w", encoding="ascii") as stream:
        stream.write("0 %d 1\n" % gid)


def _mount_tmpfs(root: str) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    mount = getattr(libc, "mount", None)
    if mount is None:
        raise OSError("mount is unavailable")
    mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_char_p]
    mount.restype = ctypes.c_int
    flags = (1 << 1) | (1 << 2) | (1 << 5)  # MS_NOSUID | MS_NODEV | MS_NOEXEC
    if mount(b"tmpfs", root.encode("utf-8"), b"tmpfs", flags, b"size=16m,mode=700") != 0:
        error = ctypes.get_errno()
        raise OSError(error, "tmpfs mount failed")


def _setup_linux_sandbox(request: SandboxRequest) -> None:
    restrictions = request.restrictions
    if not restrictions.deny_by_default or not restrictions.namespace_isolation or not restrictions.resource_limits or not restrictions.no_new_privileges:
        raise OSError("sandbox restrictions are not deny-by-default")
    if platform.system().lower() != "linux" or not callable(getattr(os, "unshare", None)):
        raise OSError("required Linux namespace support is unavailable")
    required_flags = (
        getattr(os, "CLONE_NEWUSER", 0x10000000)
        | getattr(os, "CLONE_NEWNS", 0x00020000)
        | getattr(os, "CLONE_NEWNET", 0x40000000)
        | getattr(os, "CLONE_NEWPID", 0x20000000)
    )
    os.unshare(getattr(os, "CLONE_NEWUSER", 0x10000000))
    _write_user_namespace_maps()
    os.unshare(required_flags & ~getattr(os, "CLONE_NEWUSER", 0x10000000))
    _prctl_no_new_privileges()

    # Make every mount private before replacing the visible root.  The model
    # process then has no host filesystem path to open, even if a future safe
    # builtin is accidentally added.
    libc = ctypes.CDLL(None, use_errno=True)
    mount = getattr(libc, "mount", None)
    if mount is None:
        raise OSError("mount is unavailable")
    mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_char_p]
    mount.restype = ctypes.c_int
    if mount(None, b"/", None, (1 << 14) | (1 << 18), None) != 0:  # MS_REC | MS_PRIVATE
        error = ctypes.get_errno()
        raise OSError(error, "private mount setup failed")
    root = tempfile.mkdtemp(prefix="octet-code-root-")
    _mount_tmpfs(root)
    os.chroot(root)
    os.chdir("/")

    # Apply limits inside the contained process.  A failed limit is a setup
    # failure, never a reason to continue with weaker containment.
    cpu = max(1, int(math.ceil(request.budget.max_cpu_seconds)))
    resource.setrlimit(resource.RLIMIT_CPU, (cpu, cpu))
    resource.setrlimit(resource.RLIMIT_AS, (request.budget.max_memory_bytes, request.budget.max_memory_bytes))
    resource.setrlimit(resource.RLIMIT_FSIZE, (0, 0))
    if hasattr(resource, "RLIMIT_NPROC"):
        resource.setrlimit(resource.RLIMIT_NPROC, (1, 1))
    if hasattr(resource, "RLIMIT_NOFILE"):
        resource.setrlimit(resource.RLIMIT_NOFILE, (32, 32))


def _detected_capabilities() -> SandboxCapabilities:
    system = platform.system().lower()
    prerequisites = (
        system == "linux",
        callable(getattr(os, "unshare", None)),
        callable(getattr(os, "chroot", None)),
        hasattr(resource, "setrlimit"),
    )
    genuine = all(prerequisites)
    detail = "Linux namespace, tmpfs-root, resource-limit, and no-new-privileges setup required."
    if not genuine:
        detail = "The required Linux containment primitives are unavailable."
    return SandboxCapabilities(
        genuine=genuine,
        os_isolated=genuine,
        network_isolated=genuine,
        filesystem_isolated=genuine,
        process_isolated=genuine,
        resource_limits=genuine,
        no_new_privileges=genuine,
        platform=system,
        detail=detail,
    )


# ---------------------------------------------------------------------------
# Parent-side process sandbox


class ProcessSandbox:
    """Run validated source in a namespace/resource-limited child process."""

    def __init__(self, *, capabilities: Optional[SandboxCapabilities] = None) -> None:
        self.capabilities = capabilities or _detected_capabilities()

    @property
    def available(self) -> bool:
        return self.capabilities.genuine

    def run(
        self,
        request: SandboxRequest,
        *,
        bridge_handler: Optional[Callable[[str, Mapping[str, Any]], Mapping[str, Any]]] = None,
        cancellation: Optional[Any] = None,
        timeout: Optional[float] = None,
    ) -> SandboxResult:
        if not isinstance(request, SandboxRequest):
            request = SandboxRequest(**dict(request))
        if not self.capabilities.genuine:
            return SandboxResult(
                status="sandbox_unavailable",
                error={"code": "sandbox_unavailable", "message": self.capabilities.detail or "Genuine OS containment is unavailable."},
                capabilities=self.capabilities,
            )
        if not request.restrictions.deny_by_default:
            return SandboxResult(
                status="rejected",
                error={"code": "capability_denied", "message": "Sandbox capabilities are deny-by-default."},
                capabilities=self.capabilities,
            )
        try:
            validate_source(request.source, restrictions=request.restrictions)
        except CodeRuntimeError as error:
            return SandboxResult(status="rejected", error=error.as_dict(), capabilities=self.capabilities)
        selected_timeout = request.budget.max_wall_seconds if timeout is None else timeout
        if isinstance(selected_timeout, bool) or not isinstance(selected_timeout, (int, float)) or not math.isfinite(float(selected_timeout)) or selected_timeout <= 0:
            return SandboxResult(
                status="rejected",
                error={"code": "invalid_timeout", "message": "Code-runtime timeout must be finite and positive."},
                capabilities=self.capabilities,
            )
        selected_timeout = min(float(selected_timeout), request.budget.max_wall_seconds)
        try:
            request_bytes = _json_bytes(request.as_dict(), maximum=request.budget.max_ipc_bytes)
        except CodeRuntimeError as error:
            return SandboxResult(status="rejected", error=error.as_dict(), capabilities=self.capabilities)

        try:
            context = multiprocessing.get_context("fork")
        except ValueError:
            # A spawn fallback would not make this a different kind of OS
            # sandbox, but it is intentionally not used: fork is required for
            # the currently qualified Linux setup.
            return SandboxResult(
                status="sandbox_unavailable",
                error={"code": "sandbox_unavailable", "message": "The qualified process start method is unavailable."},
                capabilities=self.capabilities,
            )
        child_to_parent, parent_to_child = context.Pipe(duplex=False)
        # For a simplex Pipe, the first endpoint receives and the second sends.
        parent_receive = child_to_parent
        child_send = parent_to_child
        parent_send, child_receive = context.Pipe(duplex=False)
        process = context.Process(
            target=_sandbox_worker,
            args=(request_bytes, child_send, child_receive, request.budget.max_ipc_bytes),
            name="octet-contained-model-code",
        )
        process.daemon = True
        try:
            process.start()
        except BaseException:
            for connection in (parent_receive, child_send, parent_send, child_receive):
                try:
                    connection.close()
                except BaseException:
                    pass
            return SandboxResult(
                status="sandbox_setup_failed",
                error={"code": "sandbox_setup_failed", "message": "The contained process could not be started."},
                capabilities=self.capabilities,
            )
        # Close child ends in the parent and parent ends in the child as soon
        # as possible.  The worker closes its inherited copies below.
        for connection in (child_send, child_receive):
            try:
                connection.close()
            except BaseException:
                pass

        deadline = time.monotonic() + selected_timeout
        in_flight: Optional[str] = None
        final: Optional[Mapping[str, Any]] = None
        setup_status: Optional[str] = None
        timed_out = False
        cancelled = False
        try:
            while True:
                if _token_cancelled(cancellation):
                    cancelled = True
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    timed_out = True
                    break
                if parent_receive.poll(min(0.025, remaining)):
                    try:
                        raw = parent_receive.recv_bytes(request.budget.max_ipc_bytes)
                        message = json.loads(raw.decode("utf-8"))
                    except (EOFError, OSError, ValueError, UnicodeDecodeError, json.JSONDecodeError):
                        final = None
                        break
                    if not isinstance(message, Mapping):
                        final = None
                        break
                    message_type = message.get("type")
                    if message_type == "sandbox_setup":
                        setup_status = str(message.get("status", "sandbox_setup_failed"))
                        continue
                    if message_type == "bridge_request":
                        operation = message.get("operation")
                        arguments = message.get("arguments", {})
                        if not isinstance(operation, str) or not isinstance(arguments, Mapping):
                            response = {
                                "type": "bridge_result",
                                "ok": False,
                                "error": {"code": "invalid_bridge_request", "message": "The bridge request was invalid."},
                            }
                        elif bridge_handler is None:
                            response = {
                                "type": "bridge_result",
                                "ok": False,
                                "error": {"code": "bridge_unavailable", "message": "No parent-owned bridge is available."},
                            }
                        else:
                            in_flight = operation
                            try:
                                raw_result = bridge_handler(operation, arguments)
                                bridge_result = _result_mapping(raw_result)
                                response = {
                                    "type": "bridge_result",
                                    "ok": True,
                                    "result": bridge_result,
                                }
                                if bridge_result.get("status") in {"unknown_effect", "unknown_effects"} or bridge_result.get("unknown_effects"):
                                    response["unknown_effect"] = True
                            except BaseException as error:
                                error_value = _error_mapping(error, "bridge_failed")
                                response = {
                                    "type": "bridge_result",
                                    "ok": False,
                                    "error": error_value,
                                    "unknown_effect": error_value.get("code") == "unknown_effect",
                                }
                            finally:
                                in_flight = None
                        try:
                            parent_send.send_bytes(_json_bytes(response, maximum=request.budget.max_ipc_bytes))
                        except (CodeRuntimeError, OSError, ValueError):
                            final = None
                            break
                        continue
                    if message_type == "final":
                        final = message
                        break
                    final = None
                    break
                if not process.is_alive() and not parent_receive.poll(0):
                    break
        finally:
            if cancelled or timed_out or final is None:
                try:
                    process.terminate()
                except BaseException:
                    pass
            process.join(timeout=min(0.25, max(0.0, deadline - time.monotonic())))
            if process.is_alive():
                try:
                    process.kill()
                except BaseException:
                    try:
                        process.terminate()
                    except BaseException:
                        pass
                process.join(timeout=0.25)
            for connection in (parent_receive, parent_send):
                try:
                    connection.close()
                except BaseException:
                    pass

        if cancelled:
            if in_flight in {"act", "act_group"}:
                return SandboxResult(
                    status="unknown_effect",
                    error={"code": "unknown_effect", "message": "Code cancellation interrupted a parent-owned action bridge."},
                    bridge_calls=0,
                    unknown_effects=({"operation": in_flight, "reason": "cancelled"},),
                    capabilities=self.capabilities,
                )
            return SandboxResult(
                status="cancelled",
                error={"code": "cancelled", "message": "Model-code execution was cancelled."},
                capabilities=self.capabilities,
            )
        if timed_out:
            if in_flight in {"act", "act_group"}:
                return SandboxResult(
                    status="unknown_effect",
                    error={"code": "unknown_effect", "message": "The code wall deadline interrupted a parent-owned action bridge."},
                    unknown_effects=({"operation": in_flight, "reason": "deadline_exceeded"},),
                    capabilities=self.capabilities,
                )
            return SandboxResult(
                status="budget_exhausted",
                error={"code": "wall_budget_exhausted", "message": "The model-code wall-time budget was exhausted."},
                capabilities=self.capabilities,
            )
        if final is None:
            status = setup_status or ("sandbox_setup_failed" if not process.exitcode else "failed")
            return SandboxResult(
                status=status,
                error={
                    "code": status,
                    "message": "The contained model-code process exited without a bounded result.",
                },
                capabilities=self.capabilities,
            )
        raw_status = final.get("status", "failed")
        status = raw_status if raw_status in _SANDBOX_STATUSES else "failed"
        unknown_value = final.get("unknown_effects", ())
        unknown: Tuple[Mapping[str, Any], ...] = ()
        if isinstance(unknown_value, (list, tuple)):
            unknown = tuple(item for item in unknown_value if isinstance(item, Mapping))
        return SandboxResult(
            status=status,
            output=final.get("output", "") if isinstance(final.get("output", ""), str) else "",
            value=final.get("value"),
            error=final.get("error") if isinstance(final.get("error"), Mapping) else None,
            bridge_calls=final.get("bridge_calls", 0) if isinstance(final.get("bridge_calls", 0), int) else 0,
            unknown_effects=unknown,
            capabilities=self.capabilities,
        )

    execute = run


def _sandbox_worker(request_bytes: bytes, child_send: Any, child_receive: Any, maximum: int) -> None:
    try:
        try:
            request_value = json.loads(request_bytes.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            _child_send(child_send, {"type": "sandbox_setup", "status": "sandbox_setup_failed"}, maximum)
            return
        if not isinstance(request_value, Mapping):
            _child_send(child_send, {"type": "sandbox_setup", "status": "sandbox_setup_failed"}, maximum)
            return
        request = SandboxRequest(
            source=request_value.get("source", ""),
            budget=CodeBudget(**dict(request_value.get("budget", {}))),
            restrictions=SandboxRestrictions(**dict(request_value.get("restrictions", {}))),
            metadata=request_value.get("metadata", {}),
        )
        try:
            _setup_linux_sandbox(request)
        except BaseException:
            _child_send(child_send, {"type": "sandbox_setup", "status": "sandbox_setup_failed"}, maximum)
            return
        _child_send(child_send, {"type": "sandbox_setup", "status": "succeeded"}, maximum)
        final = _child_execute(request.source, request_value, child_send, child_receive)
        _child_send(child_send, final, maximum)
    except BaseException:
        try:
            _child_send(child_send, {"type": "sandbox_setup", "status": "sandbox_setup_failed"}, maximum)
        except BaseException:
            pass
    finally:
        for connection in (child_send, child_receive):
            try:
                connection.close()
            except BaseException:
                pass


class OSSandbox(ProcessSandbox):
    """Explicit name for the OS-backed process sandbox."""


class UnavailableSandbox:
    """A non-executable sandbox used to make fail-closed selection explicit."""

    def __init__(self, capabilities: Optional[SandboxCapabilities] = None, *, reason: str = "") -> None:
        self.capabilities = capabilities or SandboxCapabilities(detail=reason or "Genuine OS containment is unavailable.")

    @property
    def available(self) -> bool:
        return False

    def run(self, request: SandboxRequest, **_: Any) -> SandboxResult:
        return SandboxResult(
            status="sandbox_unavailable",
            error={
                "code": "sandbox_unavailable",
                "message": self.capabilities.detail or "Genuine OS containment is unavailable.",
            },
            capabilities=self.capabilities,
        )

    execute = run


def select_sandbox(*, capabilities: Optional[SandboxCapabilities] = None) -> Any:
    selected = capabilities or _detected_capabilities()
    if not selected.genuine:
        return UnavailableSandbox(selected)
    return OSSandbox(capabilities=selected)


# ---------------------------------------------------------------------------
# Parent-owned lifecycle bridge and public runtime


def _token_cancelled(token: Any) -> bool:
    if token is None:
        return False
    value = getattr(token, "cancelled", None)
    if callable(value):
        value = value()
    if value is None:
        value = getattr(token, "is_cancelled", False)
        if callable(value):
            value = value()
    return bool(value)


def _token_reason(token: Any) -> str:
    if token is None:
        return "cancelled"
    value = getattr(token, "reason", None)
    if callable(value):
        value = value()
    return bounded_text(value or "cancelled", 96)


def _call_sandbox(
    sandbox: Any,
    request: SandboxRequest,
    *,
    bridge_handler: Callable[[str, Mapping[str, Any]], Mapping[str, Any]],
    cancellation: Optional[Any],
    timeout: Optional[float],
) -> Any:
    function = getattr(sandbox, "run", None) or getattr(sandbox, "execute", None)
    if not callable(function):
        raise CodeRuntimeError("sandbox_unavailable", "The selected sandbox has no execution method.")
    keyword = {
        "bridge_handler": bridge_handler,
        "cancellation": cancellation,
        "timeout": timeout,
    }
    try:
        signature = inspect.signature(function)
        parameters = signature.parameters
        if not any(parameter.kind == inspect.Parameter.VAR_KEYWORD for parameter in parameters.values()):
            keyword = {key: value for key, value in keyword.items() if key in parameters}
    except (TypeError, ValueError):
        pass
    return function(request, **keyword)


class CodeRuntime:
    """Parent-owned model-code runtime bound to one lifecycle manager/session."""

    def __init__(
        self,
        lifecycle: Optional[Any] = None,
        *,
        sandbox: Optional[Any] = None,
        budget: Optional[CodeBudget] = None,
        restrictions: Optional[SandboxRestrictions] = None,
    ) -> None:
        self.lifecycle = lifecycle
        self.budget = budget or CodeBudget()
        self.restrictions = restrictions or SandboxRestrictions()
        self.sandbox = sandbox or select_sandbox()

    def _owner(self, owner: Any, context: Any = None) -> OwnerIdentity:
        if owner is None:
            if isinstance(context, Mapping):
                owner = context.get("resource_owner", context.get("owner"))
            else:
                owner = getattr(context, "resource_owner", getattr(context, "owner", None)) if context is not None else None
        return OwnerIdentity.from_value(owner)

    def _parent_bridge(
        self,
        owner: OwnerIdentity,
        default_target: Optional[TargetIdentity],
        cancellation: Optional[Any],
        operation_timeout: Optional[float],
    ) -> Tuple[Callable[[str, Mapping[str, Any]], Mapping[str, Any]], Callable[[], int], Callable[[], Tuple[Mapping[str, Any], ...]]]:
        calls = 0
        unknown: List[Mapping[str, Any]] = []

        def check_target(value: Any) -> Optional[TargetIdentity]:
            if value is None:
                return default_target
            selected = TargetIdentity.from_value(value)
            if default_target is not None and selected != default_target:
                raise LifecycleError(
                    "target_reselection_required",
                    "Model code may not change the host-selected target.",
                    owner=owner,
                )
            return selected

        def bridge(operation: str, arguments: Mapping[str, Any]) -> Mapping[str, Any]:
            nonlocal calls
            if operation not in {"observe", "act", "act_group"}:
                raise CodeRuntimeError("bridge_operation_denied", "Only observe, act, and act_group are parent bridge operations.")
            if not isinstance(arguments, Mapping):
                raise CodeRuntimeError("invalid_bridge_request", "Bridge arguments must be an object.")
            calls += 1
            if calls > self.budget.max_bridge_calls:
                raise CodeRuntimeError("bridge_budget_exhausted", "The parent bridge-call budget was exhausted.")
            if self.lifecycle is None:
                raise CodeRuntimeError("bridge_unavailable", "No parent lifecycle is bound to model code.")
            values = dict(arguments)
            allowed = {
                "observe": {"target"},
                "act": {"action"},
                "act_group": {"actions", "group_id"},
            }[operation]
            if set(values) - allowed:
                raise CodeRuntimeError("invalid_bridge_request", "Unknown parent bridge fields were rejected.")
            if operation == "observe":
                target = check_target(values.get("target"))
                result = self.lifecycle.observe(
                    owner,
                    target,
                    cancellation=cancellation,
                    timeout=operation_timeout,
                )
            elif operation == "act":
                action = values.get("action")
                if not isinstance(action, Mapping):
                    raise CodeRuntimeError("invalid_action", "The act bridge requires one typed action object.")
                selected_action = dict(action)
                check_target(selected_action.get("target"))
                result = self.lifecycle.execute_group(
                    owner,
                    (selected_action,),
                    cancellation=cancellation,
                    timeout=operation_timeout,
                )
            else:
                actions = values.get("actions")
                if not isinstance(actions, (list, tuple)):
                    raise CodeRuntimeError("invalid_action_group", "The act_group bridge requires an action array.")
                for action in actions:
                    if not isinstance(action, Mapping):
                        raise CodeRuntimeError("invalid_action", "Every act_group item must be a typed action object.")
                    check_target(action.get("target"))
                group_id = values.get("group_id")
                if group_id is not None and not isinstance(group_id, str):
                    raise CodeRuntimeError("invalid_action_group", "group_id must be bounded text.")
                result = self.lifecycle.execute_group(
                    owner,
                    actions,
                    group_id=group_id,
                    cancellation=cancellation,
                    timeout=operation_timeout,
                )
            mapped = _result_mapping(result)
            if mapped.get("status") == "unknown_effect" or mapped.get("unknown_effects"):
                unknown_item: Dict[str, Any] = {"operation": operation}
                effects = mapped.get("unknown_effects")
                if isinstance(effects, list) and effects:
                    unknown_item["effects"] = effects
                unknown.append(unknown_item)
            return mapped

        return bridge, lambda: calls, lambda: tuple(unknown)

    def execute(
        self,
        owner: Any = None,
        source: Optional[str] = None,
        *,
        code: Optional[str] = None,
        context: Any = None,
        target: Optional[Any] = None,
        cancellation: Optional[Any] = None,
        timeout: Optional[float] = None,
    ) -> CodeExecutionResult:
        # ``execute(source, ...)`` is useful for direct sandbox fixtures; a
        # lifecycle-bound invocation should pass an owner explicitly or via
        # context.
        if source is None and isinstance(owner, str):
            source, owner = owner, None
        if source is None:
            source = code
        if not isinstance(source, str):
            return CodeExecutionResult(status="rejected", error={"code": "invalid_source", "message": "Model-code source must be text."})
        if timeout is not None and (isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not math.isfinite(float(timeout)) or timeout <= 0):
            return CodeExecutionResult(status="rejected", error={"code": "invalid_timeout", "message": "Code-runtime timeout must be finite and positive."})
        if _token_cancelled(cancellation):
            return CodeExecutionResult(status="cancelled", error={"code": _token_reason(cancellation), "message": "Model-code execution was cancelled."})
        try:
            validate_source(source, restrictions=self.restrictions)
            selected_owner = self._owner(owner, context) if (owner is not None or self.lifecycle is not None or context is not None) else None
            selected_target = TargetIdentity.from_value(target) if target is not None else None
        except (CodeRuntimeError, LifecycleError) as error:
            return CodeExecutionResult(status="rejected", error=_error_mapping(error, "invalid_request"))
        try:
            request = SandboxRequest(source=source, budget=self.budget, restrictions=self.restrictions)
        except CodeRuntimeError as error:
            return CodeExecutionResult(status="rejected", error=error.as_dict())
        bridge, call_count, unknown_effects = self._parent_bridge(
            selected_owner if selected_owner is not None else OwnerIdentity("code", "code", 1),
            selected_target,
            cancellation,
            timeout,
        )
        try:
            raw = _call_sandbox(
                self.sandbox,
                request,
                bridge_handler=bridge,
                cancellation=cancellation,
                timeout=timeout,
            )
            sandbox_result = raw if isinstance(raw, SandboxResult) else SandboxResult(
                status=raw.get("status", "failed") if isinstance(raw, Mapping) else "failed",
                output=raw.get("output", "") if isinstance(raw, Mapping) else "",
                value=raw.get("value") if isinstance(raw, Mapping) else None,
                error=raw.get("error") if isinstance(raw, Mapping) else {"code": "invalid_sandbox_result", "message": "The sandbox returned an invalid result."},
                bridge_calls=raw.get("bridge_calls", 0) if isinstance(raw, Mapping) else 0,
                unknown_effects=tuple(raw.get("unknown_effects", ())) if isinstance(raw, Mapping) and isinstance(raw.get("unknown_effects", ()), (list, tuple)) else (),
                capabilities=getattr(self.sandbox, "capabilities", None),
            )
        except CodeRuntimeError as error:
            return CodeExecutionResult(status="sandbox_unavailable", error=error.as_dict())
        except BaseException:
            return CodeExecutionResult(status="failed", error={"code": "sandbox_failed", "message": "The selected sandbox failed safely."})
        unknown = list(sandbox_result.unknown_effects) + list(unknown_effects())
        status = sandbox_result.status
        if unknown and status == "succeeded":
            status = "unknown_effect"
        return CodeExecutionResult(
            status=status,
            output=sandbox_result.output,
            value=sandbox_result.value,
            error=sandbox_result.error,
            bridge_calls=max(sandbox_result.bridge_calls, call_count()),
            unknown_effects=tuple(unknown),
            sandbox=sandbox_result,
        )

    run = execute
    execute_code = execute


class ContainedCodeRuntime(CodeRuntime):
    """Descriptive subclass for integrations that name the containment layer."""


class ModelCodeRuntime(ContainedCodeRuntime):
    """Descriptive model-facing runtime name."""


__all__ = [
    "CodeBudget",
    "CodeExecutionContext",
    "CodeExecutionResult",
    "CodeRuntime",
    "CodeRuntimeError",
    "CodeValidationError",
    "ContainedCodeRuntime",
    "MAX_CODE_OUTPUT_CHARS",
    "MAX_IPC_BYTES",
    "MAX_SOURCE_BYTES",
    "ModelCodeRuntime",
    "OSSandbox",
    "ProcessSandbox",
    "SandboxCapabilities",
    "SandboxRequest",
    "SandboxResult",
    "SandboxRestrictions",
    "SandboxUnavailable",
    "UnavailableSandbox",
    "select_sandbox",
    "validate_source",
]
