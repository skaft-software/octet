"""Octet tools for the pinned upstream jev-use recipe.

The upstream runner owns a separate persistent MCP session and isolated browser.
It cannot satisfy Octet's per-action confirmation policy, so that mode refuses
runs rather than treating one approval as permission for all subsequent actions.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Mapping

from octet_extension import text_content, tool_result

from . import driver, jev_use
from .jev import resolve_key

PREFIX = "computer_use_jev_use_"
_BOOL = {"type": "boolean"}
_OPTIONS = {
    "live": _BOOL,
    "typescript": _BOOL,
    "visual_fixture": _BOOL,
    "require_visual_path": _BOOL,
    "visual_observation": {"type": "string", "enum": ["auto", "always", "off"]},
    "max_steps": {"type": "integer", "minimum": 1, "maximum": 32},
    "port": {"type": "integer", "minimum": 0, "maximum": 65535},
    "expect_visual_status": {"type": "string", "enum": ["ok", "not_installed", "error", "unavailable"]},
}


def schema(properties: dict, required: tuple = ()) -> dict:
    return {"type": "object", "properties": properties,
            "additionalProperties": False, "required": list(required)}


TOOLS = {
    "status": (
        "Report pinned-source readiness when called without job_id, or poll an owned background job with job_id. Finished jobs nest the terminal tool result under result.",
        schema({"job_id": {"type": "string", "minLength": 1, "maxLength": 64}}),
    ),
    "cancel": (
        "Signal an owned jev-use background job; reports cleanup_complete false and is not rollback. Inspect status and retained evidence before retrying.",
        schema({"job_id": {"type": "string", "minLength": 1, "maxLength": 64}}, ("job_id",)),
    ),
    "setup": (
        "Start explicit setup of the pinned upstream jev-use recipe and locked dependencies as a background job; returns a job_id to poll. Downloads source and packages; does not run the browser or call Jev.",
        schema({"typescript": _BOOL}),
    ),
    "run": (
        "Start upstream jev-use end-to-end as a background job; returns a job_id to poll. Runs in its isolated browser and local form fixture. The runner observes, asks Jev (live=true) or its mock, acts, and independently verifies submission without model turns per click. Live sends compact synthetic state to TypeSafe and may incur charges. Optional visual path requires installed Cua perception. Not an arbitrary-site or native-app agent. Refused under per-action confirmation policy.",
        schema(_OPTIONS),
    ),
    "choose": (
        "Start upstream's standalone cua.jev_choice_request_v1 chooser as a background job; returns a job_id to poll. Returns a cua.jev_choice_v1 selection only; never acts. Mock avoids TypeSafe; live sends the supplied compact state to TypeSafe. Requires explicit jev-use setup.",
        schema({
            "mock": _BOOL, "typescript": _BOOL,
            "request": {"type": "object", "description": "Upstream cua.jev_choice_request_v1 document; validated by the pinned chooser. No tools, arguments, screenshot bytes or environment data.",
                        "additionalProperties": True},
        }, ("request",)),
    ),
}


def _error(message: str) -> dict:
    return tool_result(text_content(message), is_error=True)


def _validate(operation: str, values: Mapping[str, Any]) -> dict:
    if operation not in TOOLS:
        raise ValueError("Unknown jev-use operation")
    properties = TOOLS[operation][1]["properties"]
    if set(values) - set(properties):
        raise ValueError("Unknown jev-use option")
    clean = dict(values)
    for name, value in clean.items():
        kind = properties[name]["type"]
        if kind == "boolean" and not isinstance(value, bool):
            raise ValueError(name + " must be a boolean")
        if kind == "integer" and (isinstance(value, bool) or not isinstance(value, int) or not properties[name]["minimum"] <= value <= properties[name]["maximum"]):
            raise ValueError(name + " is outside its allowed range")
        if kind == "string" and "enum" in properties[name] and value not in properties[name]["enum"]:
            raise ValueError(name + " is not an allowed option")
        if kind == "object" and not isinstance(value, dict):
            raise ValueError(name + " must be an object")
    if operation == "choose" and "request" not in clean:
        raise ValueError("request is required")
    if clean.get("require_visual_path") and not clean.get("visual_fixture"):
        raise ValueError("require_visual_path requires visual_fixture")
    if clean.get("require_visual_path") and clean.get("visual_observation") == "off":
        raise ValueError("require_visual_path cannot disable visual observation")
    if clean.get("require_visual_path") and clean.get("expect_visual_status") not in (None, "ok"):
        raise ValueError("require_visual_path requires an ok visual status")
    return clean


def dispatch(operation: str, values: Mapping[str, Any], *, computer: Any,
             extension: Any, home: Any = None, gated: bool = False, cancellation: Any = None) -> dict:
    """Validate and gate before source setup, provider egress, or UI dispatch."""
    try:
        options = _validate(operation, values)
    except ValueError as error:
        return _error(str(error))
    cancellation = cancellation if cancellation is not None else extension.cancellation

    def check_cancelled() -> None:
        if cancellation is not None:
            cancellation.raise_if_cancelled()

    check_cancelled()
    if operation == "run" and gated:
        return _error("jev-use's upstream runner cannot prompt for each Driver action. "
                      "This run is refused under the active per-action confirmation policy; "
                      "use individual computer-use tools instead.")
    if operation == "setup" and gated:
        try:
            approved = extension.confirm("Install upstream jev-use?",
                                         detail="Downloads pinned Cua source and locked Python dependencies" +
                                         (" and TypeScript dependencies." if options.get("typescript") else "."),
                                         default=False)
        except Exception:
            approved = False
        if approved is not True:
            return _error("Denied: jev-use setup was not confirmed.")
    check_cancelled()
    try:
        if operation == "status":
            report = jev_use.status(home=home)
        elif operation == "setup":
            report = jev_use.setup(home=home, cancellation=cancellation, **options)
        else:
            # The key is never part of a tool argument, command line, or result.
            live = options.get("live", False) if operation == "run" else not options.get("mock", False)
            key = resolve_key() if live else None
            if live and not key:
                return _error("Live Jev requires a configured key. Set up Jev first: open /extensions, "
                              "choose octet-computer-use, then Jev.")
            if operation == "choose":
                request = options.pop("request")
                report = jev_use.choose(request, home=home, cancellation=cancellation,
                                        api_key=key, **options)
            else:
                health = driver.health(computer._paths).as_dict()
                binary = health.get("runtime_binary")
                if health.get("runtime") == "unavailable" or not binary or health.get("permissions") != "granted":
                    return _error("Cua Driver is not ready. Check computer_use_status and grant the selected runtime's permissions manually.")
                report = jev_use.run(home=home, driver_binary=Path(binary),
                                     cancellation=cancellation, api_key=key, **options)
    except Exception as error:
        # Do not echo provider/process errors that could contain credentials or
        # page contents. Cancellation remains host-owned, not a fake success.
        check_cancelled()
        return _error("jev-use failed (" + type(error).__name__ + "). Inspect its private proof/setup logs before retrying; actions may have occurred.")
    check_cancelled()
    state = report.get("status", "result")
    text = "jev-use: " + str(state)
    if "complete" in report:
        text += "; complete=" + str(report["complete"]).lower()
    if report.get("artifacts"):
        text += "; evidence: " + str(report["artifacts"])
    if report.get("error"):
        text += "; " + str(report["error"])
    failed = report.get("ok") is False or report.get("complete") is False or report.get("status") in {
        "failed", "cancelled", "timeout", "unavailable", "refused", "unknown", "error",
    }
    return tool_result(text_content(text), structured_content=report, is_error=failed)


def command_options(parts: list[str]) -> tuple[str, dict]:
    """Small exact command grammar; no arbitrary commands, paths or URLs."""
    operation = parts[0] if parts else "status"
    if operation in {"status", "cancel"} and len(parts) == 2 and not parts[1].startswith("--"):
        return operation, {"job_id": parts[1]}
    if operation not in {"status", "setup", "run"}:
        raise ValueError("Unknown jev-use operation: use status [JOB_ID], setup, run, or cancel JOB_ID with "
                         "[--typescript] [--live] [--visual-fixture] [--require-visual-path] "
                         "[--visual-observation auto|always|off] [--max-steps 1..32] [--port 0..65535] "
                         "[--expect-visual-status ok|not_installed|error|unavailable]")
    options: dict = {}
    index = 1
    while index < len(parts):
        flag = parts[index]
        name = flag.removeprefix("--").replace("-", "_")
        if not flag.startswith("--") or name in options:
            raise ValueError("Invalid or duplicate jev-use option")
        if name in {"live", "typescript", "visual_fixture", "require_visual_path"}:
            options[name] = True
        elif name in {"max_steps", "port", "visual_observation", "expect_visual_status"} and index + 1 < len(parts):
            index += 1
            options[name] = int(parts[index]) if name in {"max_steps", "port"} else parts[index]
        else:
            raise ValueError("Unknown or incomplete jev-use option")
        index += 1
    return operation, _validate(operation, options)
