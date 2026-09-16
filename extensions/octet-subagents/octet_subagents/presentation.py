"""Frontend-neutral worker tree, activity, detail, and headless formatting."""

from __future__ import annotations

import hashlib
import re
from typing import Any, Dict, List, Optional, Sequence, Tuple

from .model import Worker, bounded_text, safe_label


_ID_RE = re.compile(r"^[A-Za-z0-9_.:/-]+$")
GENERIC_STATE = {
    "queued": "pending",
    "running": "running",
    "waiting": "running",
    "stopping": "cancelled",
    "done": "succeeded",
    "limit_reached": "degraded",
    "failed": "failed",
    "stopped": "stopped",
    "timed_out": "failed",
    "cancelled": "cancelled",
    # Detached is recoverable, not terminal: it stays "degraded" (usable with
    # reduced functionality) rather than "unavailable" so no frontend renders a
    # session-owned worker as a dead resource.
    "orphaned": "degraded",
    "awaiting_approval": "pending",
    "restarted": "degraded",
}
STATE_LABEL = {
    "queued": "queued",
    "running": "running",
    "waiting": "waiting",
    "stopping": "stopping",
    "done": "done",
    "limit_reached": "limit reached",
    "failed": "failed",
    "stopped": "stopped",
    "timed_out": "timed out",
    "cancelled": "cancelled",
    # Reworded: still owned by this session, just detached from any host run.
    "orphaned": "detached",
    "awaiting_approval": "awaiting approval",
    "restarted": "restarted",
}


def semantic_id(prefix: str, value: str) -> str:
    if value and len(value.encode("utf-8")) <= 900 and _ID_RE.fullmatch(value):
        return "%s:%s" % (prefix, value)
    digest = hashlib.sha256(value.encode("utf-8", errors="replace")).hexdigest()[:24]
    return "%s:%s" % (prefix, digest)


def duration_label(elapsed_ms: int) -> str:
    total_seconds = max(0, elapsed_ms // 1000)
    hours, remainder = divmod(total_seconds, 3600)
    minutes, seconds = divmod(remainder, 60)
    if hours:
        return "%d:%02d:%02d" % (hours, minutes, seconds)
    return "%02d:%02d" % (minutes, seconds)


def human_duration(elapsed_ms: int) -> str:
    """Readable elapsed label for the live panel: `42s`, `5m49s`, `2h05m`."""
    total_seconds = max(0, elapsed_ms // 1000)
    hours, remainder = divmod(total_seconds, 3600)
    minutes, seconds = divmod(remainder, 60)
    if hours:
        return "%dh%02dm" % (hours, minutes)
    if minutes:
        return "%dm%02ds" % (minutes, seconds)
    return "%ds" % seconds


def human_tokens(value: int) -> str:
    """Bounded-width token label: exact below 10K, then `13K`, then `1.2M`."""
    if value < 10_000:
        return str(value)
    if value < 995_000:
        return "%.0fK" % (value / 1000.0)
    return "%.1fM" % (value / 1_000_000.0)


def cost_label(microdollars: Optional[int]) -> str:
    if microdollars is None:
        return "?"
    return "$%.4f" % (microdollars / 1_000_000.0)


def counts(workers: Sequence[Worker]) -> Dict[str, int]:
    values = {
        "queued": 0,
        "running": 0,
        "done": 0,
        "limited": 0,
        "failed": 0,
        "stopped": 0,
        # Detached workers are session-owned and reattachable, so they are
        # counted separately from deliberately stopped workers instead of
        # disappearing into the stopped bucket.
        "detached": 0,
    }
    for worker in workers:
        if worker.state == "queued":
            values["queued"] += 1
        elif worker.state in {"running", "waiting", "stopping", "awaiting_approval"}:
            values["running"] += 1
        elif worker.state == "done":
            values["done"] += 1
        elif worker.state == "limit_reached":
            values["limited"] += 1
        elif worker.state == "failed" or worker.state == "timed_out":
            values["failed"] += 1
        elif worker.state == "orphaned":
            values["detached"] += 1
        else:
            values["stopped"] += 1
    return values


def compact_status(workers: Sequence[Worker]) -> Tuple[str, str, Optional[str]]:
    value = counts(workers)
    pieces = []
    for key in ("running", "queued", "done", "limited", "failed", "detached", "stopped"):
        if value[key]:
            pieces.append("%d %s" % (value[key], key))
    label = "Subagents" if not pieces else "Subagents · " + " · ".join(pieces)
    if value["failed"]:
        state = "degraded"
        detail = "One or more bounded workers failed or timed out."
    elif value["limited"]:
        state = "degraded"
        detail = "One or more bounded workers reached their turn limit."
    elif value["detached"]:
        state = "degraded"
        detail = (
            "One or more workers are still owned by this session but detached "
            "from any host run; they keep their evidence and can be reattached."
        )
    elif value["running"] or value["queued"]:
        state = "active"
        detail = None
    elif workers:
        state = "active"
        detail = None
    else:
        state = "empty"
        detail = "No workers have been observed for this parent session."
    return state, bounded_text(label, 1024), detail


def safe_reference(kind: str, identifier: Optional[str], label: str) -> Optional[Dict[str, Any]]:
    if not isinstance(identifier, str) or not identifier.strip():
        return None
    if len(identifier.encode("utf-8")) > 1024 or "\x1b" in identifier:
        return None
    if any((ord(character) < 32 or 127 <= ord(character) <= 159) for character in identifier):
        return None
    return {"kind": kind, "id": identifier, "label": safe_label(label)}


def worker_references(worker: Worker) -> List[Dict[str, Any]]:
    references: List[Dict[str, Any]] = []
    session = safe_reference("session", worker.session, "Open worker transcript")
    if session is not None:
        references.append(session)
    for artifact in worker.artifacts:
        reference = safe_reference(
            "artifact", artifact.identifier, artifact.label or "Worker artifact"
        )
        if reference is not None:
            references.append(reference)
        if len(references) >= 8:
            break
    return references


def worker_secondary(worker: Worker, now_ms: int) -> str:
    """Information-only row for the live `/subagents` panel.

    Absence is never rendered as text: an inherited turn/token/cost ceiling, an
    unexposed counter, and a healthy worker's missing failure reason are simply
    omitted. Every field that remains carries information, so a panel of many
    workers cannot fill with `no ceiling`/`?` placeholders. Values are
    human-format (`5m49s`, `263K`) so a bounded row still reads as data.
    """
    pieces = [
        STATE_LABEL.get(worker.state, safe_label(worker.state)),
        human_duration(worker.elapsed_ms(now_ms)),
        "%s/%s" % (worker.profile, worker.effective_model),
    ]
    # Per-worker orchestration selection. `inherit` is the default and stays
    # absent (absence is never rendered as text); an explicit request is shown as
    # `requested→effective` so a pane-per-worker fleet is legible and a selection
    # the host has not applied is never implied to be in force.
    if worker.requested_model != "inherit":
        requested_model = "%s/%s" % (worker.requested_provider, worker.requested_model)
        marker = "" if worker.model_policy_applied else " (not applied by host)"
        pieces.append("model %s%s" % (requested_model, marker))
    if worker.requested_reasoning != "inherit":
        if not worker.model_policy_applied:
            pieces.append(
                "reasoning %s (not applied by host)" % worker.requested_reasoning
            )
        elif worker.effective_reasoning == worker.requested_reasoning:
            pieces.append("reasoning %s" % worker.requested_reasoning)
        else:
            pieces.append(
                "reasoning %s→%s"
                % (worker.requested_reasoning, worker.effective_reasoning)
            )
    if worker.reasoning_note:
        pieces.append(worker.reasoning_note)
    pieces.append(
        "%d call%s" % (worker.tool_call_count, "" if worker.tool_call_count == 1 else "s")
    )
    if worker.turn_count is not None:
        if worker.max_turns is not None:
            pieces.append("%d/%d turns" % (worker.turn_count, worker.max_turns))
        else:
            pieces.append("%d turns" % worker.turn_count)
    elif worker.max_turns is not None:
        pieces.append("max %d turns" % worker.max_turns)
    if worker.tokens_used is not None:
        used = human_tokens(worker.tokens_used)
        if worker.max_tokens is None:
            pieces.append("%s tok" % used)
        else:
            pieces.append("%s/%s tok" % (used, human_tokens(worker.max_tokens)))
    if worker.cost_microdollars is not None:
        if worker.max_cost_microdollars is None:
            pieces.append(cost_label(worker.cost_microdollars))
        else:
            pieces.append(
                "%s/%s"
                % (
                    cost_label(worker.cost_microdollars),
                    cost_label(worker.max_cost_microdollars),
                )
            )
    if worker.recovered:
        pieces.append("restarted")
    if worker.detached:
        # A detached worker is still owned by this session; say so, and say
        # whether it can be reattached, instead of rendering a terminal row.
        pieces.append("reattachable" if worker.reattachable else "reattach pending")
    if worker.reattached:
        pieces.append("reattached ×%d" % worker.reattach_count)
    if worker.awaiting_approval:
        pieces.append("approval required")
    if worker.state in {"failed", "timed_out"}:
        # A failed row without its bounded reason is useless; the reason is a
        # host-observed failure class, never tool arguments or child prose.
        reason = bounded_text(safe_label(worker.last_error or ""), 160).strip()
        if reason:
            pieces.append(reason)
    return bounded_text(" · ".join(pieces), 1024)


def _selection_text(worker: Worker) -> str:
    """One bounded line describing the requested per-worker selection.

    `inherit` is the default and is stated as such; a request the host has not
    been able to apply is stated as not applied, never implied to be in force.
    """
    requested = []
    if worker.requested_provider != "inherit" or worker.requested_model != "inherit":
        requested.append(
            "provider/model %s/%s" % (worker.requested_provider, worker.requested_model)
        )
    if worker.requested_reasoning != "inherit":
        requested.append("reasoning %s" % worker.requested_reasoning)
    if not requested:
        return "inherit the parent session's provider, model, and reasoning."
    applied = (
        "applied by the host"
        if worker.model_policy_applied
        else "requested only; the host has not confirmed a per-worker selection"
    )
    text = "%s (%s; effective %s / reasoning %s)" % (
        ", ".join(requested),
        applied,
        worker.effective_model,
        worker.effective_reasoning,
    )
    if worker.reasoning_note:
        text = "%s. %s" % (text, worker.reasoning_note)
    return text


def detail_body(worker: Worker, now_ms: int) -> str:
    tools = ", ".join(worker.tools)
    if worker.awaiting_approval:
        ownership = (
            "still owned by this parent session but detached at the approval "
            "boundary: it is parked and must not mutate unattended. Approve in an "
            "interactive octet session, then reattach with /subagents wait or "
            "subagent_continue."
        )
    elif worker.detached:
        ownership = (
            "still owned by this parent session; currently detached from any host "
            "run (a recoverable state, not a terminal one). Use /subagents wait to "
            "reattach. /subagents open-all remains Partial: pane execution is "
            "blocked until atomic host writer claim/settlement is available."
        )
    else:
        ownership = "attached to a host run owned by this parent session."
    token_use = str(worker.tokens_used) if worker.tokens_used is not None else "not exposed"
    token_limit = (
        str(worker.max_tokens)
        if worker.max_tokens is not None
        else "inherited parent setting (no session ceiling)"
    )
    lines = [
        "State: %s" % STATE_LABEL.get(worker.state, worker.state),
        "Worker: %s (%s)" % (worker.name, worker.agent_id),
        "Parentage: parent > %s; depth %d (maximum 1)" % (worker.name, worker.depth),
        "Elapsed: %s" % duration_label(worker.elapsed_ms(now_ms)),
        "Model/profile: %s (inherited) / %s" % (worker.effective_model, worker.profile),
        "Orchestration selection: %s" % _selection_text(worker),
        "Current phase/tool: %s" % safe_label(worker.current_tool or worker.phase),
        "Requested tool policy: %s"
        % (
            "read-only [%s]" % tools
            if worker.read_only
            else "granted mutation scope [%s]" % tools
        ),
        "Turn use: %s / %s"
        % (
            worker.turn_count if worker.turn_count is not None else "not exposed",
            "unlimited" if worker.max_turns is None else worker.max_turns,
        ),
        "Tool calls: %d" % worker.tool_call_count,
        "Token use: %s / %s" % (token_use, token_limit),
        "Token buckets: input %s + cache read %s + cache write %s; output %s (reasoning %s)."
        % (
            worker.input_tokens if worker.input_tokens is not None else "not exposed",
            worker.cache_read_tokens if worker.cache_read_tokens is not None else "not exposed",
            worker.cache_write_tokens if worker.cache_write_tokens is not None else "not exposed",
            worker.output_tokens if worker.output_tokens is not None else "not exposed",
            worker.reasoning_tokens if worker.reasoning_tokens is not None else "not exposed",
        ),
        "Cost use: %s / %s microdollars"
        % (
            worker.cost_microdollars
            if worker.cost_microdollars is not None
            else "not exposed",
            "unlimited"
            if worker.max_cost_microdollars is None
            else worker.max_cost_microdollars,
        ),
        "Wall deadline: %s"
        % (
            "not set (the host enforces no wall deadline)"
            if worker.deadline_at_ms is None
            else "%d ms Unix time%s"
            % (
                worker.deadline_at_ms,
                ""
                if worker.timeout_seconds is None
                else " (%d second request)" % worker.timeout_seconds,
            )
        ),
        "Cwd/workspace: inherited from the parent octet session.",
        "Sandbox/approval/environment/extensions: inherited and host-enforced; API 0.2 agent_sessions does not expose exact values to this view.",
        "Isolation: the cwd/filesystem may be shared and is not an isolation boundary.",
        "Delivery: %s; octet's durable parent mailbox owns completion claim/ack." % worker.delivery_state,
        "Session: %s" % (worker.session or "not yet exposed by agent_sessions"),
        "Session ownership: %s" % ownership,
    ]
    if worker.reattach_count:
        lines.append(
            "Reattachment: reattached %d time(s); last reattached at %s."
            % (
                worker.reattach_count,
                (
                    "%d ms Unix time" % worker.last_reattached_at_ms
                    if worker.last_reattached_at_ms is not None
                    else "an unrecorded time"
                ),
            )
        )
    elif worker.detached_at_ms is not None:
        lines.append("Detached since: %d ms Unix time." % worker.detached_at_ms)
    if worker.export_reference:
        lines.append("Export: %s" % safe_label(worker.export_reference))
    if worker.recovered:
        lines.append(
            "Restart: recovered from host-owned ancestry after %d process generation change(s)."
            % max(1, worker.restart_count)
        )
    if worker.artifacts:
        lines.append("Artifacts:")
        for artifact in worker.artifacts:
            lines.append("- %s" % safe_label(artifact.label or artifact.identifier))
    if worker.recent_tools:
        lines.append("Recent tool activity (host-observed, latest last):")
        for entry in worker.recent_tools:
            args = entry.get("args") or ""
            action = "%s %s" % (entry["name"], args) if args else str(entry["name"])
            if entry.get("finished_at_ms") is None:
                marker = "running"
            elif entry.get("error"):
                marker = "error"
            else:
                marker = "ok"
            lines.append("- [%s] %s" % (marker, action))
    if worker.summary is not None:
        lines.extend(["", "Host-observed final summary (unsafe controls escaped):", worker.summary])
    if worker.last_error is not None:
        lines.extend(["", "Last bounded error:", worker.last_error])
    return bounded_text("\n".join(lines), 64 * 1024)


def build_snapshot(
    workers: Sequence[Worker],
    *,
    selected_agent_id: Optional[str],
    now_ms: int,
) -> Dict[str, Any]:
    ordered = sorted(workers, key=lambda worker: (worker.created_at_ms, worker.agent_id))
    status_state, status_label, status_detail = compact_status(ordered)
    status: Dict[str, Any] = {"state": status_state, "label": status_label}
    if status_detail:
        status["detail"] = status_detail

    node_ids = {worker.agent_id: semantic_id("worker", worker.agent_id) for worker in ordered}
    nodes: List[Dict[str, Any]] = []
    actions: List[Dict[str, Any]] = []
    activities: List[Dict[str, Any]] = []
    for worker in ordered:
        node_id = node_ids[worker.agent_id]
        inspect_id = semantic_id("inspect", worker.agent_id)
        stop_id = semantic_id("stop", worker.agent_id)
        action_ids = [inspect_id]
        actions.append(
            {
                "id": inspect_id,
                "label": "Inspect worker",
                "command": "subagents",
                "arguments": ["inspect", worker.agent_id],
                "destructive": False,
            }
        )
        if worker.active:
            action_ids.append(stop_id)
            actions.append(
                {
                    "id": stop_id,
                    "label": "Stop worker",
                    "command": "subagents",
                    "arguments": ["stop", worker.agent_id],
                    "destructive": True,
                }
            )
        node: Dict[str, Any] = {
            "id": node_id,
            "state": GENERIC_STATE.get(worker.state, "degraded"),
            "label": safe_label(worker.name),
            "secondary": worker_secondary(worker, now_ms),
            "action_ids": action_ids,
            "references": worker_references(worker),
        }
        parent_node_id = node_ids.get(worker.parent_id or "")
        if parent_node_id is not None:
            node["parent_id"] = parent_node_id
        nodes.append(node)

        state_label = STATE_LABEL.get(worker.state, safe_label(worker.state))
        metrics: Dict[str, Any] = {
            "tool_calls": worker.tool_call_count,
            "input_tokens": worker.input_tokens or 0,
            "cache_read_tokens": worker.cache_read_tokens or 0,
            "cache_write_tokens": worker.cache_write_tokens or 0,
            "output_tokens": worker.output_tokens or 0,
            "reasoning_tokens": worker.reasoning_tokens or 0,
        }
        if worker.cost_microdollars is not None:
            metrics["cost_microdollars"] = worker.cost_microdollars
        activity: Dict[str, Any] = {
            "id": semantic_id("activity", worker.agent_id),
            "kind": "subagent",
            "state": GENERIC_STATE.get(worker.state, "degraded"),
            # Content-free: no prompt, arguments, results, or child prose.
            "summary": bounded_text("%s · %s" % (worker.name, state_label), 1024),
            "provenance": "octet agent_sessions · read-only",
            "started_at_ms": worker.started_at_ms,
            "metrics": metrics,
            "references": worker_references(worker),
        }
        if worker.completed_at_ms is not None:
            activity["completed_at_ms"] = worker.completed_at_ms
        activities.append(activity)

    selected: Optional[Worker] = None
    if selected_agent_id is not None:
        selected = next(
            (worker for worker in ordered if worker.agent_id == selected_agent_id), None
        )
    if selected is None and ordered:
        selected = ordered[-1]
    collection: Dict[str, Any] = {
        "kind": "tree",
        "title": status_label,
        "nodes": nodes,
    }
    if selected is not None:
        selected_node = node_ids[selected.agent_id]
        references = worker_references(selected)
        collection["selected_node_id"] = selected_node
        collection["detail"] = {
            "node_id": selected_node,
            "title": bounded_text("parent > %s" % selected.name, 1024),
            "body": detail_body(selected, now_ms),
            "references": references,
        }
    if any(worker.active for worker in ordered):
        actions.append(
            {
                "id": "stop-all",
                "label": "Stop all workers",
                "command": "subagents",
                "arguments": ["stop", "all"],
                "destructive": True,
            }
        )
    return {
        "status": status,
        "activities": activities[-128:],
        "collection": collection,
        "actions": actions,
    }


def narrow_list(workers: Sequence[Worker], now_ms: int) -> str:
    ordered = sorted(workers, key=lambda worker: (worker.created_at_ms, worker.agent_id))
    _, title, _ = compact_status(ordered)
    lines = [title]
    if not ordered:
        lines.append("No cached workers for this parent session.")
        return "\n".join(lines)
    for index, worker in enumerate(ordered):
        branch = "└─" if index == len(ordered) - 1 else "├─"
        lines.append(
            "%s %-20s %-10s %s  %s"
            % (
                branch,
                bounded_text(worker.name, 20),
                STATE_LABEL.get(worker.state, worker.state),
                duration_label(worker.elapsed_ms(now_ms)),
                worker.agent_id,
            )
        )
    lines.append("Use /subagents inspect <name-or-id> for cached detail.")
    lines.append("Use the model-callable subagent_stop tool for authoritative cancellation.")
    return bounded_text("\n".join(lines), 16 * 1024)
