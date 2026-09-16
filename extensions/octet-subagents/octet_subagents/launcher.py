"""Bounded escape-hatch launcher for `/subagents open-all <tmux|herdr>`.

Product pane execution is Partial: both workers and the current parent remain
blocked until atomic host writer claim/settlement exists. Opaque handles and the
low-level multiplexer adapters are retained, but a launchability snapshot is not
exclusive ownership.

House rules implemented here, in order of severity:

1. **Fail closed when the multiplexer is absent.** `shutil.which` on `PATH`
   only. octet never downloads or installs a multiplexer (the same rule this
   repository applies to `rg`/`fd`).
2. **Shell-safe.** Every session identifier, path, and flag is a separate argv
   element and `subprocess` is always called with an argv list (never
   `shell=True`). Identifiers are validated against a strict allowlist *before*
   they can reach an argv, so a metacharacter can never be smuggled in.
3. **Bounded.** At most `MAX_OPEN_ALL_PANES` (the 8-worker fleet cap plus the
   parent) panes are ever planned; beyond that the whole request is refused.
4. **No secret ever leaves this module.** Only opaque, path-free session
   references (`agent-session:<sha256>`, the contract asserted by
   `subagent_presentation_becomes_navigable_rows_with_opaque_session_references`)
   and the host-provided session id are passed on a command line. No credential,
   token, or transcript path is read, printed, or argv-passed.
5. **Clean failure.** Panes are created one at a time; the first failure stops
   the run and reports exactly what exists, without destroying anything and
   with known pane IDs and unknown outcomes retained. Inspect before retrying.

`herdr` is real and verified against its own documentation (fetched 2026-09-15):
`herdrdev/herdr` ("Terminal workspace manager for AI coding agents", https://herdr.dev,
https://herdr.dev/docs) drives panes through its CLI -- `herdr pane split --current
--direction right --cwd "$PWD" --no-focus` creates a sibling pane and returns it as
`.result.pane.pane_id`; `herdr pane run <pane-id> "<command>"` atomically sends a
command *string* plus Enter (https://raw.githubusercontent.com/herdrdev/herdr/master/skills/herdr/SKILL.md).
Because herdr has no argv-list pane executor the string form is only ever assembled
from tokens that already passed `_SAFE_TOKEN_RE` -- a token with any shell
metacharacter is rejected before the string exists. herdr documents a hard guardrail
for agents: control commands require `HERDR_ENV=1`, i.e. this process must already be
inside a Herdr-managed pane ("Herdr blocks nested launches by design",
https://herdr.dev/agent-guide.md), so open-all refuses to drive a herdr session it
does not own.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
from dataclasses import dataclass, field
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .model import MAX_ACTIVE_CHILDREN, SubagentError, bounded_text

MULTIPLEXERS = ("tmux", "herdr")
# One pane per running worker (the host fleet cap) plus the parent pane.
MAX_OPEN_ALL_PANES = MAX_ACTIVE_CHILDREN + 1
OPAQUE_SESSION_PREFIX = "agent-session:"
MAX_PANE_LABEL_BYTES = 64
MAX_COMMAND_BYTES = 8 * 1024
COMMAND_TIMEOUT_SECONDS = 15

# Session file stems, as handed to the extension in `host.session_id`.
_SESSION_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,199}$")
# The host's path-free delegated session reference.
_SESSION_REFERENCE_RE = re.compile(r"^agent-session:[0-9a-f]{64}$")
# Conservative shell-safe allowlist: the characters a token must be built from
# before it may appear inside a single multiplexer command string.
_SAFE_TOKEN_RE = re.compile(r"^[A-Za-z0-9_@%+=:,./-]{1,512}$")
_DEFAULT_FLEET_SESSION = "octet-fleet"


def _is_control(character: str) -> bool:
    codepoint = ord(character)
    return codepoint < 32 or 127 <= codepoint <= 159


def validate_session_id(value: Any, *, name: str) -> str:
    """Validate the host session stem that `octet --resume` resolves."""
    if not isinstance(value, str) or _SESSION_ID_RE.fullmatch(value) is None:
        raise SubagentError(
            "%s is not a launchable octet session id" % name,
            code="unlaunchable_session",
        )
    return value


def validate_session_reference(value: Any, *, name: str) -> str:
    """Validate the opaque, path-free delegated-session reference.

    Anything else -- including a raw path or an id carrying a shell
    metacharacter -- is refused, so this value can never widen a command line.
    """
    if not isinstance(value, str) or _SESSION_REFERENCE_RE.fullmatch(value) is None:
        raise SubagentError(
            "%s is not an opaque agent-session reference" % name,
            code="unlaunchable_session",
        )
    return value


def validate_working_directory(value: Any) -> Optional[str]:
    if value is None:
        return None
    if not isinstance(value, str) or not value:
        raise SubagentError("workspace must be an absolute path", code="invalid_workspace")
    if not value.startswith("/") and not (len(value) > 2 and value[1] == ":"):
        raise SubagentError("workspace must be an absolute path", code="invalid_workspace")
    if len(value.encode("utf-8")) > 4096 or any(_is_control(character) for character in value):
        raise SubagentError("workspace contains control characters", code="invalid_workspace")
    return value


def validate_label(value: Any) -> str:
    text = bounded_text(" ".join(str(value).split()), MAX_PANE_LABEL_BYTES)
    if any(character in ";|&$`'\"\\<>()*?[]{}!#\n" for character in text):
        raise SubagentError("pane label contains shell metacharacters", code="invalid_label")
    if not text:
        raise SubagentError("pane label is empty", code="invalid_label")
    return text


def detect_binary(binary: str) -> Optional[str]:
    """Return the absolute path of an already-installed binary, else None.

    Read-only discovery: this function never installs, downloads, or writes.
    """
    if not isinstance(binary, str) or not binary:
        return None
    found = shutil.which(binary)
    if not found:
        return None
    return found


def _octet_binary() -> str:
    override = os.environ.get("OCTET_SUBAGENTS_OCTET_BIN")
    if isinstance(override, str) and override and not any(
        character in override for character in ";|&$`'\"\\<>()*?[]{}!#\n "
    ):
        return override
    found = detect_binary("octet")
    if found is None:
        raise SubagentError(
            "the `octet` executable is not on PATH, so there is nothing to reopen; "
            "put the octet binary on PATH (octet never downloads or installs it)",
            code="octet_missing",
        )
    return found


@dataclass(frozen=True)
class Pane:
    """One planned pane, including explicit blocked ownership rows."""

    role: str
    name: str
    handle_kind: str
    handle: str
    argv: Tuple[str, ...]
    resolvable: bool
    blocked_reason: Optional[str] = None

    @property
    def label(self) -> str:
        return validate_label("%s-%s" % (self.role, self.name))

    def plan_row(self) -> Dict[str, Any]:
        return {
            "role": self.role,
            "name": self.name,
            "handle_kind": self.handle_kind,
            "argv": list(self.argv),
            "resolvable": self.resolvable,
            "blocked_reason": self.blocked_reason,
        }


@dataclass(frozen=True)
class LaunchPlan:
    multiplexer: str
    binary: str
    panes: Tuple[Pane, ...]
    session_name: Optional[str] = None
    inside_multiplexer: bool = True
    workspace: Optional[str] = None

    @property
    def executable(self) -> Tuple[Pane, ...]:
        return tuple(pane for pane in self.panes if pane.resolvable)

    @property
    def blocked(self) -> Tuple[Pane, ...]:
        return tuple(pane for pane in self.panes if not pane.resolvable)


@dataclass
class LaunchOutcome:
    multiplexer: str
    created: List[Dict[str, Any]] = field(default_factory=list)
    blocked: List[Dict[str, Any]] = field(default_factory=list)
    failure: Optional[Dict[str, Any]] = None
    notices: List[str] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return self.failure is None


ATOMIC_WRITER_CLAIM_BLOCKED_REASON = (
    "atomic host writer claim/settlement unavailable; a launchability snapshot "
    "is not exclusive ownership. Worker pane execution is blocked until the "
    "host provides an atomic writer claim/settlement primitive"
)
WORKER_PANE_BLOCKED_REASON = (
    "the host has not confirmed that this worker is launchable with no live task; "
    "refresh through an owner-bound /subagents command for current status. "
    "Pane execution still requires atomic host writer claim/settlement"
)
PARENT_PANE_BLOCKED_REASON = (
    "the current process still owns the parent session; opening it with --resume "
    "would create a second writer. Parent handover requires host-owned settlement "
    "and an exclusive ownership transfer, which this command cannot perform"
)
LIVE_WORKER_BLOCKED_REASON = (
    "a live worker owns this session; stop it and wait for authoritative settlement "
    "before opening another writer"
)
# A session-owned worker that is not attached to any run gets no pane: the
# extension holds no live handle for it, and open-all never opens a stale one.
# Both reasons name the recoverable state and the reattach affordance so the row
# can never become a silent omission.
DETACHED_WORKER_NOT_OPENED_REASON = (
    "still owned by this session but detached from any host run, so its live "
    "session is not addressable and no stale pane is opened for it. Reattach it "
    "with /subagents wait or subagent_status, then re-run open-all; a reattached "
    "worker is planned again."
)
PARKED_WORKER_NOT_OPENED_REASON = (
    "parked by the host at the approval boundary, so no pane is opened for it: "
    "driving a parked worker from a new pane would be unattended mutation. "
    "Approve it in an interactive session (or stop it explicitly), then re-run "
    "open-all."
)


def resolve_parent_pane(
    *,
    parent_session_id: Optional[str],
    octet_binary: str,
    multiplexer: str,
    workspace: Optional[str],
    inside: bool,
    session_name: Optional[str],
    first: bool,
) -> Pane:
    handle = validate_session_id(parent_session_id, name="the host parent session id")
    argv = _pane_argv(
        multiplexer=multiplexer,
        octet_binary=octet_binary,
        handle=handle,
        label="parent",
        workspace=workspace,
        inside=inside,
        session_name=session_name,
        first=first,
    )
    return Pane(
        role="parent",
        name="parent",
        handle_kind="host_session_id",
        handle=handle,
        argv=argv,
        resolvable=False,
        blocked_reason=PARENT_PANE_BLOCKED_REASON,
    )


def worker_blocked_reason(worker: Any) -> str:
    if getattr(worker, "state", None) == "awaiting_approval":
        return PARKED_WORKER_NOT_OPENED_REASON
    if getattr(worker, "live_task", None) is True:
        return LIVE_WORKER_BLOCKED_REASON
    if not getattr(worker, "host_present", False):
        return DETACHED_WORKER_NOT_OPENED_REASON
    reason = getattr(worker, "launch_blocked", None)
    if isinstance(reason, str) and reason:
        return bounded_text(reason, 512)
    if (getattr(worker, "launchable", False) is not True
            or getattr(worker, "live_task", None) is not False):
        return WORKER_PANE_BLOCKED_REASON
    return ATOMIC_WRITER_CLAIM_BLOCKED_REASON


def resolve_worker_pane(
    worker: Any,
    *,
    octet_binary: str,
    multiplexer: str,
    workspace: Optional[str],
    inside: bool,
    session_name: Optional[str],
    first: bool,
) -> Pane:
    name = validate_label(getattr(worker, "name", "worker"))
    reference = getattr(worker, "session", None)
    if reference is None:
        raise SubagentError(
            "worker %s has no host session reference yet; wait for the host to "
            "publish one before opening a pane for it" % name,
            code="unlaunchable_session",
        )
    handle = validate_session_reference(reference, name="the worker session reference")
    argv = _pane_argv(
        multiplexer=multiplexer,
        octet_binary=octet_binary,
        handle=handle,
        label=name,
        workspace=workspace,
        inside=inside,
        session_name=session_name,
        first=first,
    )
    return Pane(
        role="worker",
        name=name,
        handle_kind="opaque_session_reference",
        handle=handle,
        argv=argv,
        # Host launchability is only an observation, never a writer claim.
        resolvable=False,
        blocked_reason=worker_blocked_reason(worker),
    )


def _pane_argv(
    *,
    multiplexer: str,
    octet_binary: str,
    handle: str,
    label: str,
    workspace: Optional[str],
    inside: bool,
    session_name: Optional[str],
    first: bool,
) -> Tuple[str, ...]:
    """Build the exact argv for one pane. No shell, no interpolation."""
    command = [octet_binary, "--resume", handle]
    label = validate_label(label)
    if multiplexer == "tmux":
        argv = ["tmux"]
        if inside:
            # Reuse the session the operator is already in: one window per session.
            argv += ["new-window", "-d"]
        elif first:
            argv += ["new-session", "-d", "-s", validate_label(session_name or _DEFAULT_FLEET_SESSION)]
        else:
            argv += ["new-window", "-d", "-t", validate_label(session_name or _DEFAULT_FLEET_SESSION)]
        argv += ["-n", label]
        if workspace:
            argv += ["-c", workspace]
        argv += ["--"]
        argv += command
        return tuple(argv)
    if multiplexer == "herdr":
        # herdr has no argv-list pane executor: the split is one command and the
        # command string is submitted separately (see `run_herdr`).
        return ("__herdr__",) + tuple(command)
    raise SubagentError(
        "unsupported multiplexer %r; use one of: %s"
        % (multiplexer, ", ".join(MULTIPLEXERS)),
        code="unsupported_multiplexer",
    )


def plan_open_all(
    *,
    multiplexer: Any,
    parent_session_id: Optional[str],
    workers: Sequence[Any],
    workspace: Optional[str],
    environment: Optional[Mapping[str, str]] = None,
) -> LaunchPlan:
    """Retain opaque handles and blocked rows; no product pane is executable."""
    environment = os.environ if environment is None else environment
    if not isinstance(multiplexer, str) or multiplexer not in MULTIPLEXERS:
        raise SubagentError(
            "open-all requires a multiplexer: %s" % " or ".join(MULTIPLEXERS),
            code="unsupported_multiplexer",
        )
    binary = detect_binary(multiplexer)
    if binary is None:
        raise SubagentError(
            "%s is not installed on PATH, so no pane can be opened; octet never "
            "downloads or installs a multiplexer -- install %s yourself and retry"
            % (multiplexer, multiplexer),
            code="multiplexer_missing",
        )
    if multiplexer == "herdr" and environment.get("HERDR_ENV") != "1":
        # Mirrors herdr's documented agent guardrail: an agent outside a
        # herdr-managed pane must not drive a session it does not own.
        raise SubagentError(
            "HERDR_ENV=1 is not set, so this process is not inside a herdr-managed "
            "pane; refusing to drive a herdr session it does not own",
            code="multiplexer_not_owner",
        )
    candidates = [worker for worker in workers
                  if getattr(worker, "active", False) or getattr(worker, "launchable", False) is True]
    planned = 1 + len(candidates)
    if planned > MAX_OPEN_ALL_PANES:
        raise SubagentError(
            "open-all would plan %d panes, above the documented cap of %d "
            "(the parent plus the %d-worker fleet cap); nothing was opened"
            % (planned, MAX_OPEN_ALL_PANES, MAX_ACTIVE_CHILDREN),
            code="pane_cap",
        )
    octet_binary = _octet_binary()
    workspace = validate_working_directory(
        workspace if workspace else environment.get("OCTET_WORKSPACE")
    )
    inside = bool(environment.get("TMUX")) if multiplexer == "tmux" else True
    session_name = (
        None
        if inside
        else "%s-%s"
        % (
            _DEFAULT_FLEET_SESSION,
            validate_session_id(parent_session_id, name="the host parent session id")[:32],
        )
    )
    panes: List[Pane] = [
        resolve_parent_pane(
            parent_session_id=parent_session_id,
            octet_binary=octet_binary,
            multiplexer=multiplexer,
            workspace=workspace,
            inside=inside,
            session_name=session_name,
            first=True,
        )
    ]
    for worker in candidates:
        panes.append(
            resolve_worker_pane(
                worker,
                octet_binary=octet_binary,
                multiplexer=multiplexer,
                workspace=workspace,
                inside=inside,
                session_name=session_name,
                # Display-only argv: no planned row grants execution authority.
                first=len(panes) == 1,
            )
        )
    handles = [pane.handle for pane in panes if pane.role == "worker"]
    if len(set(handles)) != len(handles):
        raise SubagentError("duplicate worker session handles; nothing was opened", code="invalid_launch")
    plan = LaunchPlan(
        multiplexer=multiplexer,
        binary=binary,
        panes=tuple(panes),
        session_name=session_name,
        inside_multiplexer=inside,
        workspace=workspace,
    )
    _preflight(plan)
    return plan


def _validate_command(argv: Sequence[str]) -> None:
    """Validate all local bounds before any multiplexer effect."""
    if not isinstance(argv, (list, tuple)) or not argv:
        raise SubagentError("refusing to run an empty command", code="invalid_launch")
    if any(not isinstance(token, str) or not token for token in argv):
        raise SubagentError("refusing a command with an empty argv element", code="invalid_launch")
    if len(" ".join(argv).encode("utf-8")) > MAX_COMMAND_BYTES:
        raise SubagentError("refusing an oversized launch command", code="invalid_launch")


def _run(argv: Sequence[str]) -> subprocess.CompletedProcess:
    _validate_command(argv)
    return subprocess.run(  # noqa: S603 - argv list, shell=False, bounded timeout
        list(argv),
        shell=False,
        capture_output=True,
        text=True,
        timeout=COMMAND_TIMEOUT_SECONDS,
        check=False,
    )


def _herdr_command(argv: Sequence[str]) -> str:
    """Join an argv into herdr's single command string, metacharacter-free.

    `herdr pane run` takes one command string, so this is the only place where
    tokens are joined. Every token must already match the shell-safe allowlist;
    otherwise the request is refused and no string is produced.
    """
    tokens = [token for token in argv if isinstance(token, str) and token]
    if len(tokens) != len(argv):
        raise SubagentError("refusing a malformed herdr command", code="invalid_launch")
    for token in tokens:
        if _SAFE_TOKEN_RE.fullmatch(token) is None:
            raise SubagentError(
                "refusing a herdr command token with shell metacharacters",
                code="unsafe_command_token",
            )
    _validate_command(tokens)
    return " ".join(tokens)


def _preflight(plan: LaunchPlan) -> None:
    for pane in plan.executable:
        if plan.multiplexer == "herdr":
            command = _herdr_command(pane.argv[1:])
            # Reserve the maximum pane-id length before creating a pane.
            _validate_command(["herdr", "pane", "run", "p" * 512, command])
            _validate_command(_herdr_split(plan.workspace))
        else:
            _validate_command(pane.argv)


def _herdr_pane_id(stdout: Any) -> str:
    try:
        value = json.loads(stdout if isinstance(stdout, str) else "")
    except (TypeError, ValueError):
        raise SubagentError(
            "herdr pane split returned malformed JSON; a pane may exist but its id is unknown",
            code="multiplexer_protocol",
        )
    result = value.get("result") if isinstance(value, dict) else None
    pane = result.get("pane") if isinstance(result, dict) else None
    pane_id = pane.get("pane_id") if isinstance(pane, dict) else None
    if not isinstance(pane_id, str) or _SAFE_TOKEN_RE.fullmatch(pane_id) is None:
        raise SubagentError(
            "herdr pane split returned no usable pane id; a pane may exist but its id is unknown",
            code="multiplexer_protocol",
        )
    return pane_id


def _open_tmux_pane(pane: Pane) -> Dict[str, Any]:
    result = _run(pane.argv)
    if result.returncode != 0:
        return {
            "pane": pane.plan_row(),
            "returncode": result.returncode,
            "stderr": bounded_text((result.stderr or "").strip(), 512),
        }
    return {"pane": pane.plan_row(), "returncode": 0}


def _herdr_split(workspace: Optional[str]) -> List[str]:
    split = ["herdr", "pane", "split", "--current", "--direction", "right", "--no-focus"]
    if workspace:
        split += ["--cwd", workspace]
    return split


def _open_herdr_pane(pane: Pane, *, workspace: Optional[str]) -> Dict[str, Any]:
    # Validate the command BEFORE splitting. Parsing/submission can still fail
    # after an effect; retain the pane id and distinguish creation from launch.
    command = _herdr_command(pane.argv[1:])
    row: Dict[str, Any] = {"pane": pane.plan_row(), "returncode": None,
                           "pane_created": None, "command_submitted": False}
    try:
        result = _run(_herdr_split(workspace))
        row["returncode"] = result.returncode
        if result.returncode != 0:
            row["stderr"] = bounded_text((result.stderr or "split failed").strip(), 512)
            return row
        row["returncode"] = None
        pane_id = _herdr_pane_id(result.stdout)
        row.update(pane_id=pane_id, pane_created=True, command_submitted=None)
        submitted = _run(["herdr", "pane", "run", pane_id, command])
        row["returncode"] = submitted.returncode
        # A failed acknowledgement does not prove that Enter was never sent.
        row["command_submitted"] = True if submitted.returncode == 0 else None
        if submitted.returncode != 0:
            row["stderr"] = bounded_text((submitted.stderr or "command submission failed").strip(), 512)
    except subprocess.TimeoutExpired:
        row["stderr"] = "herdr did not answer within %ds; the last operation may have taken effect" % COMMAND_TIMEOUT_SECONDS
    except SubagentError as error:
        row["stderr"] = str(error)
    except OSError:
        row["stderr"] = "herdr could not be executed; inspect existing panes before retrying"
    return row


def execute_plan(plan: LaunchPlan, *, workspace: Optional[str] = None) -> LaunchOutcome:
    """Create the resolvable panes one by one; stop cleanly at the first failure."""
    _preflight(plan)
    outcome = LaunchOutcome(multiplexer=plan.multiplexer)
    for pane in plan.panes:
        if not pane.resolvable:
            outcome.blocked.append(
                {
                    "pane": pane.plan_row(),
                    "reason": pane.blocked_reason,
                }
            )
            continue
        try:
            created = (
                _open_tmux_pane(pane)
                if plan.multiplexer == "tmux"
                else _open_herdr_pane(pane, workspace=plan.workspace)
            )
        except subprocess.TimeoutExpired:
            created = {
                "pane": pane.plan_row(),
                "returncode": None,
                "stderr": "the multiplexer did not answer within %ds"
                % COMMAND_TIMEOUT_SECONDS,
            }
        except OSError:
            created = {"pane": pane.plan_row(), "returncode": None,
                       "stderr": "the multiplexer could not be executed"}
        except SubagentError as error:
            created = {
                "pane": pane.plan_row(),
                "returncode": None,
                "stderr": str(error),
            }
        if created.get("returncode") != 0:
            if created.get("pane_created") is True:
                outcome.created.append(created)
            outcome.failure = created
            outcome.notices.append(
                "open-all stopped at pane %s/%s; panes already created were left "
                "untouched. The failed operation may have taken effect; inspect "
                "existing panes before re-running to avoid duplicate writers." % (pane.role, pane.name)
            )
            return outcome
        outcome.created.append(created)
    if outcome.blocked:
        outcome.notices.append(
            "%d pane(s) were planned but not opened; see each blocked-pane reason."
            % len(outcome.blocked)
        )
    return outcome


def open_all(
    *,
    multiplexer: Any,
    parent_session_id: Optional[str],
    workers: Sequence[Any],
    workspace: Optional[str],
    environment: Optional[Mapping[str, str]] = None,
) -> Tuple[LaunchPlan, Optional[LaunchOutcome]]:
    """Plan, then execute. Planning failures (`multiplexer_missing`, pane cap,
    unsafe identifier) raise before anything is created."""
    plan = plan_open_all(
        multiplexer=multiplexer,
        parent_session_id=parent_session_id,
        workers=workers,
        workspace=workspace,
        environment=environment,
    )
    outcome = execute_plan(plan, workspace=workspace)
    return plan, outcome


def skipped_worker_row(worker: Any) -> Dict[str, Any]:
    """Describe a session-owned worker that deliberately gets no pane.

    A detached worker is still alive and owned by this session, so dropping it
    from the open-all report would be a silent omission; a worker parked at the
    host approval boundary must never be opened as if it were live. Neither is
    opened, and both are named with the recoverable next step.
    """
    state = str(getattr(worker, "state", "") or "orphaned")
    name = bounded_text(
        " ".join(str(getattr(worker, "name", "worker")).split()), MAX_PANE_LABEL_BYTES
    )
    return {
        "id": getattr(worker, "agent_id", None),
        "name": name,
        "state": state,
        "reattachable": bool(getattr(worker, "reattachable", False)),
        "reason": (
            worker_blocked_reason(worker) or WORKER_PANE_BLOCKED_REASON
        ),
    }


def render_outcome(
    plan: LaunchPlan,
    outcome: LaunchOutcome,
    *,
    skipped: Sequence[Mapping[str, Any]] = (),
) -> str:
    """Operator-facing, bounded, secret-free report of what actually happened."""
    lines = [
        "open-all %s: %d pane(s) created, %d blocked, %d not opened, %s"
        % (
            plan.multiplexer,
            len(outcome.created),
            len(outcome.blocked),
            len(skipped),
            ("blocked (Partial)" if outcome.blocked else "clean") if outcome.ok else "stopped early",
        )
    ]
    for created in outcome.created:
        pane = created["pane"]
        if created.get("returncode") == 0:
            lines.append("- opened %s pane (%s); interactive startup is not confirmed" % (pane["role"], pane["name"]))
        else:
            lines.append("- created %s pane (%s), id %s; command submission failed or is unconfirmed"
                         % (pane["role"], pane["name"], created["pane_id"]))
    if outcome.failure is not None:
        pane = outcome.failure["pane"]
        if outcome.failure.get("pane_created") is None:
            lines.append("- pane creation may have taken effect; inspect the multiplexer before retrying")
        lines.append(
            "- FAILED %s pane (%s): %s"
            % (pane["role"], pane["name"], outcome.failure.get("stderr") or "no detail")
        )
    for blocked in outcome.blocked:
        pane = blocked["pane"]
        lines.append(
            "- blocked %s pane (%s): %s"
            % (pane["role"], pane["name"], blocked["reason"] or "no reason recorded")
        )
    for row in skipped:
        lines.append(
            "- not opened %s worker (%s): %s"
            % (
                "parked" if row.get("state") == "awaiting_approval" else row.get("state", "unknown"),
                row.get("name"),
                row.get("reason") or "no reason recorded",
            )
        )
    for notice in outcome.notices:
        lines.append(notice)
    lines.append(
        "The read-only parent-controlled /subagents panel is unchanged. "
        "Product pane execution remains Partial: atomic host writer "
        "claim/settlement unavailable."
    )
    return bounded_text("\n".join(lines), 16 * 1024)


def plan_argv_rows(plan: LaunchPlan) -> List[Dict[str, Any]]:
    return [pane.plan_row() for pane in plan.panes]
