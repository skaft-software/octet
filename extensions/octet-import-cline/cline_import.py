"""Bounded, read-only Cline setup conversion for the API 0.3 process adapter.

This module deliberately treats every source file as inert JSON or UTF-8 text.  It
never imports source modules, expands settings, starts commands, or writes to the
source or destination.  The public functions return plain API 0.3 wire-shaped
values so the process wrapper does not need a Cline SDK.
"""

from __future__ import annotations

import json
import os
import stat
import unicodedata
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Optional, Sequence


MAX_CONFIG_BYTES = 1024 * 1024
MAX_MCP_CONFIG_BYTES = 256 * 1024
MAX_SKILL_BYTES = 128 * 1024
MAX_SOURCE_ITEMS = 128
MAX_PATH_BYTES = 4096
MAX_NAME_BYTES = 128
MAX_COMMAND_BYTES = 4096
MAX_ARGUMENT_BYTES = 16384
MAX_DIAGNOSTIC_BYTES = 4096
MAX_JSON_DEPTH = 32


_WINDOWS_RESERVED_NAMES = {
    "CON",
    "PRN",
    "AUX",
    "NUL",
    *(f"COM{index}" for index in range(1, 10)),
    *(f"LPT{index}" for index in range(1, 10)),
}
_WINDOWS_FORBIDDEN_COMPONENT_CHARS = set('<>:"|?*')


def _portable_path_component(value: str) -> bool:
    """Reject names that are source-relative on POSIX but unsafe on Windows."""
    if any(character in _WINDOWS_FORBIDDEN_COMPONENT_CHARS for character in value):
        return False
    if value.endswith((".", " ")):
        return False
    return value.split(".", 1)[0].upper() not in _WINDOWS_RESERVED_NAMES


class AdapterError(ValueError):
    """An input or source-boundary error safe to expose as invalid_params."""


class _MissingSourcePath(Exception):
    pass


class _UnreadableSourcePath(Exception):
    pass


class _InvalidSourceJson(Exception):
    pass


# The order is intentional.  The first entries are the names used by the Cline
# VS Code extension and its global-storage settings directory; the dot-directory
# and generic MCP layouts are compatibility paths, not recursive auto-discovery.
_CONFIG_CANDIDATES = (
    ("globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json", "mcp"),
    ("globalStorage/saoudrizwan.claude-dev/settings/cline_settings.json", "settings"),
    ("globalStorage/saoudrizwan.claude-dev/settings/cline_custom_modes.json", "settings"),
    ("globalStorage/saoudrizwan.claude-dev/cline_mcp_settings.json", "mcp"),
    ("cline_mcp_settings.json", "mcp"),
    ("cline_settings.json", "settings"),
    ("cline_custom_modes.json", "settings"),
    ("settings/cline_mcp_settings.json", "mcp"),
    ("settings/cline_settings.json", "settings"),
    ("settings/cline_custom_modes.json", "settings"),
    (".cline/mcp.json", "mcp"),
    (".cline/cline_mcp_settings.json", "mcp"),
    (".cline/settings.json", "settings"),
    (".vscode/mcp.json", "mcp"),
    ("settings.json", "settings"),
)

# Cline rules are intentionally separated from arbitrary source files.  A
# structured skills root accepts <name>/SKILL.md; a rules root also accepts
# bounded direct Markdown files because .clinerules historically used that
# layout.
_SKILL_ROOT_CANDIDATES = (
    (".cline/skills", "structured"),
    (".cline/rules", "rules"),
    (".clinerules", "rules"),
    ("skills", "structured"),
    ("globalStorage/saoudrizwan.claude-dev/skills", "structured"),
    ("globalStorage/saoudrizwan.claude-dev/settings/skills", "structured"),
)

_MODEL_PAIRS = (
    ("apiProvider", "apiModelId"),
    ("actModeApiProvider", "actModeApiModelId"),
    ("planModeApiProvider", "planModeApiModelId"),
)

# These are Cline's provider-specific model-id fields.  The provider names are
# taken from the setting key itself; no provider is inferred from an arbitrary
# model string.
_PROVIDER_MODEL_KEYS = {
    "anthropic": "anthropicModelId",
    "openai": "openAiModelId",
    "openai-native": "openAiNativeModelId",
    "openrouter": "openRouterModelId",
    "gemini": "geminiModelId",
    "deepseek": "deepSeekModelId",
    "mistral": "mistralModelId",
    "xai": "xaiModelId",
    "ollama": "ollamaModelId",
    "lmstudio": "lmStudioModelId",
    "litellm": "liteLlmModelId",
    "requesty": "requestyModelId",
    "moonshot": "moonshotModelId",
    "qwen": "qwenModelId",
    "vertex": "vertexModelId",
    "bedrock": "bedrockModelId",
}

_SECRET_KEY_PARTS = (
    "apikey",
    "api_key",
    "accesstoken",
    "access_token",
    "accesskey",
    "access_key",
    "authtoken",
    "auth_token",
    "authorization",
    "auth",
    "clientsecret",
    "client_secret",
    "privatekey",
    "private_key",
    "oauth",
    "password",
    "credential",
    "secret",
    "token",
)

_PERMISSION_KEYS = {
    "autoApprove",
    "alwaysAllow",
    "disabled",
    "enabled",
    "approvalRequired",
    "requireApproval",
    "auto_approve",
    "always_allow",
    "approval_required",
    "require_approval",
    "autoApprovalSettings",
    "auto_approval_settings",
    "approvalMode",
    "approval_mode",
    "permissionMode",
    "permission_mode",
    "toolPermissions",
    "tool_permissions",
    "allowTools",
    "allowedTools",
    "denyTools",
    "deniedTools",
}

_SENSITIVE_ARGUMENT_FLAGS = {
    "--api-key",
    "--apikey",
    "--token",
    "--access-token",
    "--access_token",
    "--client-secret",
    "--client_secret",
    "--private-key",
    "--private_key",
    "--credential",
    "--credentials",
    "--password",
    "--secret",
    "--authorization",
    "--auth",
    "--auth-token",
    "--auth_token",
    "--header",
    "--headers",
    "-e",
    "--env",
    "--env-file",
    "--environment",
}

_SENSITIVE_ARGUMENT_EXACT_FLAGS = {"-H"}

_SENSITIVE_ARGUMENT_MARKERS = (
    "api_key=",
    "api-key=",
    "apikey=",
    "access_token=",
    "access-token=",
    "accesstoken=",
    "access_key=",
    "access-key=",
    "accesskey=",
    "client_secret=",
    "client-secret=",
    "clientsecret=",
    "private_key=",
    "private-key=",
    "privatekey=",
    "password=",
    "authorization=",
    "authorization:",
    "api-key:",
    "x-api-key=",
    "x-api-key:",
    "x-auth-token=",
    "x-auth-token:",
    "x-access-token=",
    "x-access-token:",
    "token=",
    "secret=",
    "credential=",
    "credentials=",
    "oauth=",
    "oauth:",
    "bearer ",
    "basic ",
    "--api-key=",
    "--apikey=",
    "--token=",
    "--access-token=",
    "--access_token=",
    "--client-secret=",
    "--client_secret=",
    "--private-key=",
    "--private_key=",
    "--password=",
    "--secret=",
    "--authorization=",
    "--auth-token=",
    "--auth_token=",
    "--header=",
    "--headers=",
    "-e=",
    "--env=",
    "--env-file=",
    "--environment=",
)

_UNSAFE_COMMAND_NAMES = {"env", "printenv", "export", "set"}

_UNSUPPORTED_SETTINGS_KEYS = {
    "customInstructions",
    "custom_instructions",
    "customModes",
    "custom_modes",
    "autoApprovalSettings",
    "auto_approval_settings",
    "autoApprove",
    "alwaysAllow",
    "disabled",
    "enabled",
    "approvalRequired",
    "requireApproval",
    "auto_approve",
    "always_allow",
    "approval_required",
    "require_approval",
    "approvalMode",
    "approval_mode",
    "permissionMode",
    "permission_mode",
    "toolPermissions",
    "tool_permissions",
    "allowTools",
    "allowedTools",
    "denyTools",
    "deniedTools",
    "browserSettings",
    "browser_settings",
    "diffEnabled",
    "diff_enabled",
    "telemetrySetting",
    "telemetry_setting",
    "requestTimeout",
    "request_timeout",
    "env",
    "environment",
    "environmentVariables",
    "environment_variables",
    "envVars",
    "env_vars",
    "headers",
    "requestHeaders",
    "request_headers",
    "cwd",
    "workingDirectory",
    "working_directory",
    "permissions",
    "permission",
}


# Fixed diagnostic prose avoids copying source values into the protocol.  Paths
# remain source-relative provenance, as required by the migration contract.
_REASON_CONFIG_READ = "A Cline configuration file could not be read and was skipped."
_REASON_CONFIG_GONE = "A configuration file reported during detection is no longer present."
_REASON_CONFIG_JSON = "The Cline configuration is not valid JSON and was not imported."
_REASON_CONFIG_ROOT = "The Cline configuration root must be an object and was not imported."
_REASON_MODEL_INCOMPLETE = "A Cline model selection was incomplete and was not imported."
_REASON_MODEL_INVALID = "A Cline model selection exceeded migration safety bounds and was not imported."
_REASON_SETTINGS_UNSUPPORTED = "Cline settings outside model selection were not imported."
_REASON_CREDENTIALS = "Cline credentials and authentication state were not imported."
_REASON_SKILL_ROOT = "A Cline skill directory was not a regular directory and was skipped."
_REASON_SKILL_READ = "A Cline skill could not be read and was not imported."
_REASON_SKILL_UTF8 = "A Cline skill is not UTF-8 and was not imported."
_REASON_SKILL_BOUNDS = "Cline skill discovery reached its bounded item limit."
_REASON_MCP_ENTRY = "A Cline MCP entry is not an object and was not imported."
_REASON_MCP_ENV = "MCP environment variables and headers were not imported."
_REASON_MCP_CWD = "MCP working directories were not imported; configure them after review."
_REASON_MCP_PERMISSIONS = "MCP enablement and approval settings were not imported."
_REASON_MCP_TRANSPORT = "Only local stdio MCP servers can be imported."
_REASON_MCP_COMMAND = "An MCP server without a direct command was not imported."
_REASON_MCP_ARGS = "An MCP server has invalid or non-string arguments and was not imported."
_REASON_MCP_SECRET_ARG = "An MCP server argument looked credential-like and was not imported."
_REASON_MCP_UNSAFE_COMMAND = "An MCP server used an unsafe environment command and was not imported."
_REASON_MCP_BOUNDS = "An MCP server exceeded migration safety bounds and was not imported."
_REASON_SOURCE_ITEMS = "Cline source discovery reached its bounded item limit."


def _utf8_size(value: str) -> int:
    return len(value.encode("utf-8", "strict"))


def _safe_text(value: Any, maximum: int) -> Optional[str]:
    if not isinstance(value, str) or not value:
        return None
    try:
        if _utf8_size(value) > maximum:
            return None
    except UnicodeError:
        return None
    if any(character.isspace() and character in "\x00\n\r\t" for character in value):
        return None
    if any(unicodedata.category(character) in {"Cc", "Cf"} for character in value):
        return None
    return value


def _valid_path_text(value: Any) -> bool:
    if value == "$":
        return True
    if _safe_text(value, MAX_PATH_BYTES) is None:
        return False
    assert isinstance(value, str)
    if value.startswith("/") or "\\" in value:
        return False
    parts = value.split("/")
    if any(not part or part in (".", "..") for part in parts):
        return False
    if any(not _portable_path_component(part) for part in parts):
        return False
    return PurePosixPath(value).as_posix() == value


def _add_diagnostic(
    diagnostics: list[dict[str, str]], path: str, severity: str, reason: str
) -> None:
    if len(diagnostics) >= MAX_SOURCE_ITEMS:
        return
    if not _valid_path_text(path):
        path = "$"
    if severity not in ("warning", "error"):
        severity = "warning"
    if _safe_text(reason, MAX_DIAGNOSTIC_BYTES) is None:
        reason = "The Cline adapter skipped a source item."
    diagnostics.append({"path": path, "severity": severity, "reason": reason})


def _source_root(value: Any) -> Path:
    source_text = _safe_text(value, MAX_PATH_BYTES)
    if source_text is None:
        raise AdapterError("source_root must be a bounded non-empty string")
    source = Path(source_text)
    if not source.is_absolute():
        raise AdapterError("source_root must be absolute")
    # Validate every lexical component, not just the final directory entry.
    # This prevents an intermediate symlink from escaping the explicitly
    # authorized source root.
    try:
        current = Path(source.anchor)
        for part in source.parts[1:]:
            if part in ("", ".", ".."):
                raise AdapterError("source_root must use normalized path components")
            current /= part
            metadata = os.lstat(current)
            if stat.S_ISLNK(metadata.st_mode):
                raise AdapterError("source_root must not contain symlinks")
    except AdapterError:
        raise
    except OSError as error:
        raise AdapterError("source_root does not exist") from error
    try:
        metadata = os.lstat(source)
    except OSError as error:
        raise AdapterError("source_root does not exist") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise AdapterError("source_root must be a regular directory")
    try:
        canonical = source.resolve(strict=True)
        canonical_metadata = os.lstat(canonical)
    except OSError as error:
        raise AdapterError("source_root does not exist") from error
    if stat.S_ISLNK(canonical_metadata.st_mode) or not stat.S_ISDIR(canonical_metadata.st_mode):
        raise AdapterError("source_root must be a regular directory")
    return canonical


def _relative_source_path(root: Path, path: Path) -> str:
    try:
        relative = path.relative_to(root)
    except ValueError as error:
        raise _UnreadableSourcePath from error
    parts = relative.parts
    if not parts or any(part in ("", ".", "..") for part in parts):
        raise _UnreadableSourcePath
    result = "/".join(parts)
    if not _valid_path_text(result):
        raise _UnreadableSourcePath
    return result


def _safe_existing_path(root: Path, relative: str) -> Path:
    if not _valid_path_text(relative) or relative == "$":
        raise _UnreadableSourcePath
    parts = relative.split("/")
    if any(not part or part in (".", "..") for part in parts):
        raise _UnreadableSourcePath
    # Validate every path component with lstat.  A final-entry check alone
    # would permit a symlinked directory component to redirect source reads.
    lexical = root
    for index, part in enumerate(parts):
        lexical = lexical / part
        try:
            metadata = os.lstat(lexical)
        except FileNotFoundError as error:
            raise _MissingSourcePath from error
        except OSError as error:
            raise _UnreadableSourcePath from error
        if stat.S_ISLNK(metadata.st_mode):
            raise _UnreadableSourcePath
        if index < len(parts) - 1 and not stat.S_ISDIR(metadata.st_mode):
            raise _UnreadableSourcePath
    try:
        canonical = lexical.resolve(strict=True)
        canonical.relative_to(root)
    except (OSError, ValueError) as error:
        raise _UnreadableSourcePath from error
    return canonical


def _read_optional_regular(root: Path, relative: str, limit: int) -> Optional[bytes]:
    try:
        path = _safe_existing_path(root, relative)
    except _MissingSourcePath:
        return None
    except _UnreadableSourcePath as error:
        raise _UnreadableSourcePath from error
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise _UnreadableSourcePath from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise _UnreadableSourcePath
    try:
        with path.open("rb") as handle:
            data = handle.read(limit + 1)
    except OSError as error:
        raise _UnreadableSourcePath from error
    if len(data) > limit:
        raise _UnreadableSourcePath
    return data


def _reject_constant(value: str) -> None:
    raise _InvalidSourceJson(value)


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _InvalidSourceJson
        result[key] = value
    return result


def _check_source_value(value: Any) -> None:
    # Use an explicit stack so a bounded input cannot trigger a Python recursion
    # error before the adapter can turn it into a source diagnostic.
    pending: list[tuple[Any, int]] = [(value, 1)]
    while pending:
        current, depth = pending.pop()
        if depth > MAX_JSON_DEPTH:
            raise _InvalidSourceJson
        if isinstance(current, str):
            try:
                current.encode("utf-8", "strict")
            except UnicodeError as error:
                raise _InvalidSourceJson from error
        elif isinstance(current, dict):
            for key, child in current.items():
                if not isinstance(key, str):
                    raise _InvalidSourceJson
                pending.append((key, depth + 1))
                pending.append((child, depth + 1))
        elif isinstance(current, list):
            pending.extend((child, depth + 1) for child in current)
        elif isinstance(current, (int, float, bool)) or current is None:
            continue
        else:
            raise _InvalidSourceJson


def _parse_source_json(data: bytes) -> Any:
    try:
        value = json.loads(
            data.decode("utf-8", "strict"),
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=_reject_constant,
        )
        _check_source_value(value)
        return value
    except (UnicodeError, ValueError, TypeError, RecursionError) as error:
        raise _InvalidSourceJson from error


def _candidate_limit(kind: str) -> int:
    return MAX_MCP_CONFIG_BYTES if kind == "mcp" else MAX_CONFIG_BYTES


def _read_candidate(
    root: Path, relative: str, kind: str
) -> tuple[Optional[bytes], Optional[str]]:
    try:
        return _read_optional_regular(root, relative, _candidate_limit(kind)), None
    except (_MissingSourcePath, _UnreadableSourcePath):
        return None, _REASON_CONFIG_READ


def _record_skill_entry(
    found: list[tuple[str, str]], seen: set[str], name: str, relative: str, diagnostics: list[dict[str, str]]
) -> None:
    if len(found) >= MAX_SOURCE_ITEMS:
        return
    if relative == "$" or not _valid_path_text(relative):
        _add_diagnostic(diagnostics, "$", "warning", _REASON_SKILL_READ)
        return
    valid_name = _safe_text(name, MAX_NAME_BYTES)
    if valid_name is None:
        _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_READ)
        return
    if relative in seen:
        return
    seen.add(relative)
    found.append((valid_name, relative))


def _bounded_directory_entries(path: Path, relative: str, diagnostics: list[dict[str, str]]) -> list[os.DirEntry[str]]:
    entries: list[os.DirEntry[str]] = []
    overflow = False
    try:
        with os.scandir(path) as iterator:
            for index, entry in enumerate(iterator):
                if index >= MAX_SOURCE_ITEMS:
                    overflow = True
                    break
                entries.append(entry)
    except OSError:
        _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_ROOT)
        return []
    if overflow:
        _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_BOUNDS)
    entries.sort(key=lambda entry: entry.name)
    return entries


def _discover_structured_skills(
    root: Path,
    root_relative: str,
    directory: Path,
    found: list[tuple[str, str]],
    seen: set[str],
    diagnostics: list[dict[str, str]],
    visited: list[int],
    depth: int = 0,
) -> None:
    if len(found) >= MAX_SOURCE_ITEMS or visited[0] >= MAX_SOURCE_ITEMS or depth > 2:
        return
    visited[0] += 1
    for entry in _bounded_directory_entries(directory, root_relative, diagnostics):
        if len(found) >= MAX_SOURCE_ITEMS:
            return
        try:
            metadata = entry.stat(follow_symlinks=False)
        except OSError:
            continue
        entry_relative = f"{root_relative}/{entry.name}"
        if stat.S_ISLNK(metadata.st_mode):
            _add_diagnostic(diagnostics, entry_relative, "warning", _REASON_SKILL_ROOT)
            continue
        if not stat.S_ISDIR(metadata.st_mode):
            continue
        skill_file = Path(entry.path) / "SKILL.md"
        try:
            skill_metadata = os.lstat(skill_file)
        except OSError:
            skill_metadata = None
        if skill_metadata is not None and stat.S_ISLNK(skill_metadata.st_mode):
            _add_diagnostic(
                diagnostics, f"{entry_relative}/SKILL.md", "warning", _REASON_SKILL_READ
            )
        if skill_metadata is not None and stat.S_ISREG(skill_metadata.st_mode):
            relative = f"{entry_relative}/SKILL.md"
            _record_skill_entry(found, seen, entry.name, relative, diagnostics)
            continue
        if depth < 2:
            _discover_structured_skills(
                root,
                entry_relative,
                Path(entry.path),
                found,
                seen,
                diagnostics,
                visited,
                depth + 1,
            )


def _discover_rules_skills(
    root: Path,
    root_relative: str,
    directory: Path,
    found: list[tuple[str, str]],
    seen: set[str],
    diagnostics: list[dict[str, str]],
    visited: list[int],
    depth: int = 0,
) -> None:
    if len(found) >= MAX_SOURCE_ITEMS or visited[0] >= MAX_SOURCE_ITEMS or depth > 2:
        return
    visited[0] += 1
    for entry in _bounded_directory_entries(directory, root_relative, diagnostics):
        if len(found) >= MAX_SOURCE_ITEMS:
            return
        try:
            metadata = entry.stat(follow_symlinks=False)
        except OSError:
            continue
        entry_relative = f"{root_relative}/{entry.name}"
        if stat.S_ISLNK(metadata.st_mode):
            _add_diagnostic(diagnostics, entry_relative, "warning", _REASON_SKILL_ROOT)
            continue
        if stat.S_ISREG(metadata.st_mode):
            suffix = Path(entry.name).suffix.lower()
            if suffix in (".md", ".markdown"):
                _record_skill_entry(found, seen, Path(entry.name).stem, entry_relative, diagnostics)
            continue
        if stat.S_ISDIR(metadata.st_mode):
            skill_file = Path(entry.path) / "SKILL.md"
            try:
                skill_metadata = os.lstat(skill_file)
            except OSError:
                skill_metadata = None
            if skill_metadata is not None and stat.S_ISLNK(skill_metadata.st_mode):
                _add_diagnostic(
                    diagnostics, f"{entry_relative}/SKILL.md", "warning", _REASON_SKILL_READ
                )
            if skill_metadata is not None and stat.S_ISREG(skill_metadata.st_mode):
                _record_skill_entry(found, seen, entry.name, f"{entry_relative}/SKILL.md", diagnostics)
            elif depth < 2:
                _discover_rules_skills(
                    root,
                    entry_relative,
                    Path(entry.path),
                    found,
                    seen,
                    diagnostics,
                    visited,
                    depth + 1,
                )


def _discover_skills(root: Path, diagnostics: list[dict[str, str]]) -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    seen: set[str] = set()
    visited = [0]
    for relative, kind in _SKILL_ROOT_CANDIDATES:
        if len(found) >= MAX_SOURCE_ITEMS:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SOURCE_ITEMS)
            break
        try:
            path = _safe_existing_path(root, relative)
        except _MissingSourcePath:
            continue
        except _UnreadableSourcePath:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_ROOT)
            continue
        try:
            metadata = os.lstat(path)
        except OSError:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_ROOT)
            continue
        if stat.S_ISLNK(metadata.st_mode):
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_ROOT)
            continue
        if stat.S_ISREG(metadata.st_mode):
            if relative == ".clinerules" or kind == "rules":
                name = Path(relative).stem
                _record_skill_entry(found, seen, name, relative, diagnostics)
            continue
        if not stat.S_ISDIR(metadata.st_mode):
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_ROOT)
            continue
        if kind == "structured":
            _discover_structured_skills(root, relative, path, found, seen, diagnostics, visited)
        else:
            _discover_rules_skills(root, relative, path, found, seen, diagnostics, visited)
    if visited[0] >= MAX_SOURCE_ITEMS and len(found) < MAX_SOURCE_ITEMS:
        _add_diagnostic(diagnostics, "$", "warning", _REASON_SKILL_BOUNDS)
    found.sort(key=lambda item: item[1])
    return found[:MAX_SOURCE_ITEMS]


def _contains_sensitive_key(value: Any) -> bool:
    pending: list[Any] = [value]
    while pending:
        current = pending.pop()
        if isinstance(current, dict):
            for key, child in current.items():
                normalized = key.replace("-", "_").lower()
                if any(part in normalized for part in _SECRET_KEY_PARTS):
                    return True
                pending.append(child)
        elif isinstance(current, list):
            pending.extend(current)
    return False


def _contains_key(value: Any, keys: set[str]) -> bool:
    normalized_keys = {key.replace("-", "_").lower() for key in keys}
    pending: list[Any] = [value]
    while pending:
        current = pending.pop()
        if isinstance(current, dict):
            if any(key.replace("-", "_").lower() in normalized_keys for key in current):
                return True
            pending.extend(current.values())
        elif isinstance(current, list):
            pending.extend(current)
    return False


def _settings_objects(value: dict[str, Any]) -> Iterable[dict[str, Any]]:
    yield value
    nested = value.get("apiConfiguration")
    if isinstance(nested, dict) and nested is not value:
        yield nested
    nested = value.get("api_configuration")
    if isinstance(nested, dict) and nested is not value:
        yield nested


def _append_model(
    models: list[dict[str, str]], diagnostics: list[dict[str, str]], path: str, provider: Any, model: Any
) -> bool:
    provider_text = _safe_text(provider, MAX_NAME_BYTES)
    model_text = _safe_text(model, MAX_NAME_BYTES)
    if provider_text is None or model_text is None:
        _add_diagnostic(diagnostics, path, "warning", _REASON_MODEL_INVALID)
        return False
    provider_text = provider_text.strip()
    model_text = model_text.strip()
    if not provider_text or not model_text:
        _add_diagnostic(diagnostics, path, "warning", _REASON_MODEL_INVALID)
        return False
    identity = (provider_text.casefold(), model_text)
    if any(
        (existing["provider"].strip().casefold(), existing["model"].strip()) == identity
        for existing in models
    ):
        return False
    if len(models) >= MAX_SOURCE_ITEMS:
        return False
    models.append({"path": path, "provider": provider_text, "model": model_text})
    return True


def _extract_models(
    value: dict[str, Any], path: str, models: list[dict[str, str]], diagnostics: list[dict[str, str]]
) -> None:
    model_setting_seen = False
    for settings in _settings_objects(value):
        for provider_key, model_key in _MODEL_PAIRS:
            has_provider = provider_key in settings
            has_model = model_key in settings
            if not has_provider and not has_model:
                continue
            model_setting_seen = True
            if not has_provider or not has_model:
                _add_diagnostic(diagnostics, path, "warning", _REASON_MODEL_INCOMPLETE)
                continue
            _append_model(models, diagnostics, path, settings.get(provider_key), settings.get(model_key))

        provider = settings.get("apiProvider")
        if isinstance(provider, str):
            provider_key = provider.strip().lower()
            model_key = _PROVIDER_MODEL_KEYS.get(provider_key)
            if model_key is not None and model_key in settings:
                model_setting_seen = True
                _append_model(models, diagnostics, path, provider, settings.get(model_key))

        # A provider-specific model field is only accepted with its explicit
        # provider name; this loop never guesses from the model identifier.
        for provider_name, model_key in _PROVIDER_MODEL_KEYS.items():
            if model_key not in settings or "apiProvider" in settings:
                continue
            model_setting_seen = True
            _append_model(models, diagnostics, path, provider_name, settings.get(model_key))

    if _contains_sensitive_key(value):
        _add_diagnostic(diagnostics, path, "warning", _REASON_CREDENTIALS)
    if _contains_key(value, _UNSUPPORTED_SETTINGS_KEYS):
        _add_diagnostic(diagnostics, path, "warning", _REASON_SETTINGS_UNSUPPORTED)
    if not model_setting_seen and (
        "apiProvider" in value
        or "apiModelId" in value
        or "apiConfiguration" in value
        or "api_configuration" in value
    ):
        _add_diagnostic(diagnostics, path, "warning", _REASON_MODEL_INCOMPLETE)


def _looks_like_url_server(server: dict[str, Any]) -> bool:
    return any(
        key in server
        for key in (
            "url",
            "uri",
            "endpoint",
            "endpointUrl",
            "endpoint_url",
            "serverUrl",
            "server_url",
            "httpUrl",
            "http_url",
            "sseUrl",
            "sse_url",
            "streamableHttpUrl",
            "streamable_http_url",
        )
    )


def _sensitive_flag(value: str) -> bool:
    stripped = value.strip()
    lowered = stripped.lower()
    if stripped in _SENSITIVE_ARGUMENT_EXACT_FLAGS or lowered in _SENSITIVE_ARGUMENT_FLAGS:
        return True
    # Short options commonly carry their value without a separator.  Preserve
    # Cline's exact-case -H rule while treating -e and long options
    # case-insensitively.
    if stripped.startswith("-H") and len(stripped) > 2:
        return True
    if stripped.startswith("-e") and len(stripped) > 2:
        return True
    return any(
        lowered.startswith(f"{flag}=") or lowered.startswith(f"{flag}:")
        for flag in _SENSITIVE_ARGUMENT_FLAGS
        if flag.startswith("--")
    )


def _looks_like_environment_assignment(value: str) -> bool:
    name, separator, _ = value.strip().partition("=")
    if not separator or not name or not (name[0].isalpha() or name[0] == "_"):
        return False
    return all(character.isalnum() or character == "_" for character in name)


def _sensitive_argument(value: str) -> bool:
    lowered = value.strip().lower()
    return (
        _sensitive_flag(value)
        or _looks_like_environment_assignment(value)
        or any(marker in lowered for marker in _SENSITIVE_ARGUMENT_MARKERS)
    )


def _unsafe_command(value: str) -> bool:
    command = value.strip().casefold().replace("\\", "/").rsplit("/", 1)[-1]
    parts = command.split(None, 1)
    if not parts:
        return False
    command = parts[0].strip("\"'")
    if command.endswith(".exe"):
        command = command[:-4]
    return command in _UNSAFE_COMMAND_NAMES


def _extract_mcp(
    value: dict[str, Any], path: str, servers: list[dict[str, Any]], diagnostics: list[dict[str, str]]
) -> None:
    values: Any = None
    found_key = False
    for key in ("mcpServers", "mcp_servers", "servers"):
        if key in value:
            values = value[key]
            found_key = True
            break
    if not found_key:
        return
    if not isinstance(values, dict):
        _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_ENTRY)
        return

    permission_reported = False
    env_reported = False
    cwd_reported = False
    credentials_reported = False
    for name in sorted(values):
        server = values[name]
        if not isinstance(server, dict):
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_ENTRY)
            continue
        if _contains_sensitive_key(server) and not credentials_reported:
            _add_diagnostic(diagnostics, path, "warning", _REASON_CREDENTIALS)
            credentials_reported = True
        if _contains_key(server, {"env", "headers", "requestHeaders", "request_headers"}) and not env_reported:
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_ENV)
            env_reported = True
        if _contains_key(server, {"cwd", "workingDirectory", "working_directory"}) and not cwd_reported:
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_CWD)
            cwd_reported = True
        if _contains_key(server, _PERMISSION_KEYS) and not permission_reported:
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_PERMISSIONS)
            permission_reported = True

        transport = None
        for transport_key in ("transport", "transportType", "transport_type", "type"):
            if transport_key in server:
                transport = server[transport_key]
                break
        if transport is not None and transport != "stdio":
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_TRANSPORT)
            continue
        if _looks_like_url_server(server):
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_TRANSPORT)
            continue

        command = _safe_text(server.get("command"), MAX_COMMAND_BYTES)
        if command is None or not command.strip():
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_COMMAND)
            continue
        raw_args = server.get("args", [])
        if not isinstance(raw_args, list):
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_ARGS)
            continue
        if len(raw_args) > MAX_SOURCE_ITEMS:
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_BOUNDS)
            continue
        args: list[str] = []
        invalid_args = False
        previous_sensitive_flag = False
        for raw_arg in raw_args:
            argument = _safe_text(raw_arg, MAX_ARGUMENT_BYTES)
            if argument is None:
                invalid_args = True
                break
            if previous_sensitive_flag or _sensitive_argument(argument):
                _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_SECRET_ARG)
                invalid_args = True
                break
            args.append(argument)
            previous_sensitive_flag = _sensitive_flag(argument)
        if invalid_args:
            continue
        if _unsafe_command(command):
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_UNSAFE_COMMAND)
            continue
        if _sensitive_argument(command):
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_SECRET_ARG)
            continue
        name_text = _safe_text(name, MAX_NAME_BYTES)
        if name_text is None:
            _add_diagnostic(diagnostics, path, "warning", _REASON_MCP_BOUNDS)
            continue
        if len(servers) >= MAX_SOURCE_ITEMS:
            continue
        servers.append({"path": path, "name": name_text, "command": command, "args": args})


def _config_paths_and_diagnostics(root: Path) -> tuple[list[str], list[dict[str, str]]]:
    paths: list[str] = []
    diagnostics: list[dict[str, str]] = []
    for relative, kind in _CONFIG_CANDIDATES:
        if len(paths) >= MAX_SOURCE_ITEMS:
            _add_diagnostic(diagnostics, "$", "warning", _REASON_SOURCE_ITEMS)
            break
        try:
            data = _read_optional_regular(root, relative, _candidate_limit(kind))
        except _MissingSourcePath:
            continue
        except _UnreadableSourcePath:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_CONFIG_READ)
            continue
        if data is not None:
            paths.append(relative)
    return paths, diagnostics


def detect(source_root: str) -> dict[str, Any]:
    """Return a bounded API 0.3 MigrationDetectResult."""
    root = _source_root(source_root)
    config_paths, diagnostics = _config_paths_and_diagnostics(root)
    skills = _discover_skills(root, diagnostics)
    return {
        "detected": bool(config_paths or skills),
        "config_paths": config_paths[:MAX_SOURCE_ITEMS],
        "diagnostics": diagnostics[:MAX_SOURCE_ITEMS],
    }


def _authorized_config_path(relative: Any, seen: set[str]) -> str:
    if not isinstance(relative, str) or not _valid_path_text(relative):
        raise AdapterError("config_paths must contain bounded source-relative paths")
    allowed = {candidate for candidate, _kind in _CONFIG_CANDIDATES}
    if relative not in allowed or relative in seen:
        raise AdapterError("config_paths contains an unauthorized or duplicate path")
    seen.add(relative)
    return relative


def import_setup(source_root: str, config_paths: Sequence[str]) -> dict[str, Any]:
    """Convert the explicitly detected Cline paths without side effects."""
    root = _source_root(source_root)
    if not isinstance(config_paths, (list, tuple)) or len(config_paths) > MAX_SOURCE_ITEMS:
        raise AdapterError("config_paths exceeds the migration item bound")

    models: list[dict[str, str]] = []
    skills: list[dict[str, str]] = []
    mcp_servers: list[dict[str, Any]] = []
    diagnostics: list[dict[str, str]] = []
    seen: set[str] = set()

    for raw_relative in config_paths:
        relative = _authorized_config_path(raw_relative, seen)
        kind = next(kind for candidate, kind in _CONFIG_CANDIDATES if candidate == relative)
        try:
            data = _read_optional_regular(root, relative, _candidate_limit(kind))
        except _MissingSourcePath:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_CONFIG_GONE)
            continue
        except _UnreadableSourcePath:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_CONFIG_READ)
            continue
        if data is None:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_CONFIG_GONE)
            continue
        try:
            value = _parse_source_json(data)
        except _InvalidSourceJson:
            _add_diagnostic(diagnostics, relative, "error", _REASON_CONFIG_JSON)
            continue
        if not isinstance(value, dict):
            _add_diagnostic(diagnostics, relative, "error", _REASON_CONFIG_ROOT)
            continue
        _extract_models(value, relative, models, diagnostics)
        _extract_mcp(value, relative, mcp_servers, diagnostics)

    for name, relative in _discover_skills(root, diagnostics):
        if len(skills) >= MAX_SOURCE_ITEMS:
            break
        try:
            data = _read_optional_regular(root, relative, MAX_SKILL_BYTES)
        except (_MissingSourcePath, _UnreadableSourcePath):
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_READ)
            continue
        if data is None:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_READ)
            continue
        try:
            content = data.decode("utf-8", "strict")
        except UnicodeError:
            _add_diagnostic(diagnostics, relative, "warning", _REASON_SKILL_UTF8)
            continue
        skills.append({"path": relative, "name": name, "content": content})

    return {
        "models": models[:MAX_SOURCE_ITEMS],
        "skills": skills[:MAX_SOURCE_ITEMS],
        "mcp_servers": mcp_servers[:MAX_SOURCE_ITEMS],
        "diagnostics": diagnostics[:MAX_SOURCE_ITEMS],
    }


# Descriptive aliases make the adapter functions convenient to exercise without
# coupling tests or future callers to the JSON-RPC process wrapper's names.
detect_source = detect
import_source = import_setup


__all__ = ["AdapterError", "detect", "detect_source", "import_setup", "import_source"]
