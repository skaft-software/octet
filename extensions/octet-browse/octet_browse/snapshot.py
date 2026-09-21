"""Bounded, value-redacting semantic browser snapshots and references."""

from __future__ import annotations

from dataclasses import dataclass, field
import unicodedata
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple
from urllib.parse import quote, quote_plus

from .safety import BrowseError, bounded_text, sanitize_url
from .targeting import SnapshotReference, inspect_target


MAX_SNAPSHOT_CHARS = 20_000
MAX_INTERACTIVE_ELEMENTS = 100
MAX_BODY_SOURCE_CHARS = 30_000
MAX_BODY_OUTPUT_CHARS = 30_000
MAX_BODY_TRAVERSAL_NODES = 10_000
MAX_SNAPSHOT_GENERATION = (2**53) - 1
UNTRUSTED_BEGIN = "BEGIN UNTRUSTED BROWSER CONTENT"
UNTRUSTED_END = "END UNTRUSTED BROWSER CONTENT"

# Native selectors precede ARIA fallbacks and explicitly exclude their native
# equivalents so one element is not assigned multiple references.
INTERACTIVE_GROUPS: Sequence[Tuple[str, str]] = (
    ("a[href]", "link"),
    ("button", "button"),
    ('input:not([type="hidden"])', "textbox"),
    ("textarea", "textbox"),
    ("select", "combobox"),
    ('[role="button"]:not(button)', "button"),
    ('[role="link"]:not(a)', "link"),
    ('[role="textbox"]:not(input):not(textarea)', "textbox"),
    ('[role="searchbox"]:not(input)', "searchbox"),
    ('[role="checkbox"]:not(input)', "checkbox"),
    ('[role="radio"]:not(input)', "radio"),
    ('[role="combobox"]:not(select)', "combobox"),
    ('[role="switch"]', "switch"),
    ('[role="tab"]', "tab"),
    ('[role="menuitem"]', "menuitem"),
)

# Only numeric budgets enter the page realm, never remembered typed values.
# substringData bounds even a single enormous text node. The iterative walk
# neither recurses into deep trees nor copies an unbounded childNodes array.
_VISIBLE_BODY_EXTRACTOR = r"""
(body, options) => {
  const maxSource = options.max_source;
  const maxOutput = options.max_output;
  const maxNodes = options.max_nodes;
  const skippedTags = new Set([
    "SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "INPUT", "TEXTAREA",
    "SELECT", "OPTION", "DATALIST"
  ]);
  const blockDisplays = new Set([
    "block", "flow-root", "list-item", "flex", "grid", "table",
    "table-row", "table-caption", "table-row-group", "table-header-group",
    "table-footer-group"
  ]);
  let output = "";
  let sourceCount = 0;
  let traversedNodes = 0;
  let separator = "";
  let sourceTruncated = false;
  let outputTruncated = false;
  let traversalTruncated = false;
  let editablePresent = false;
  const stack = [{ node: body, entered: false }];

  while (stack.length) {
    const frame = stack[stack.length - 1];
    const node = frame.node;
    if (!frame.entered) {
      if (traversedNodes === maxNodes) {
        traversalTruncated = true;
        break;
      }
      traversedNodes += 1;
      frame.entered = true;
      const type = node.nodeType;
      // Closed details hide all but their first summary without giving those
      // hidden descendants display:none in their own computed styles.
      if (frame.details) {
        if (type !== Node.ELEMENT_NODE || node.tagName !== "SUMMARY" || frame.details.summarySeen) {
          stack.pop();
          continue;
        }
        frame.details.summarySeen = true;
      }
      if (type === Node.TEXT_NODE) {
        const length = node.length;
        let offset = 0;
        while (offset < length) {
          // Defer layout separators until actual text follows, so a cut ends
          // in the source prefix that the host must redact, not an added LF.
          const prefix = output && !output.endsWith("\n") ? separator : "";
          const sourceLeft = maxSource - sourceCount;
          const outputLeft = maxOutput - output.length - prefix.length;
          sourceTruncated = sourceLeft === 0;
          outputTruncated = outputLeft <= 0;
          if (sourceTruncated || outputTruncated) break;
          const size = Math.min(1024, length - offset, sourceLeft, outputLeft);
          const chunk = node.substringData(offset, size);
          output += prefix + chunk;
          separator = "";
          sourceCount += size;
          offset += size;
        }
        if (sourceTruncated || outputTruncated) break;
        stack.pop();
        continue;
      }
      if (type !== Node.ELEMENT_NODE || skippedTags.has(node.tagName) ||
          node.hasAttribute("hidden") || node.getAttribute("aria-hidden") === "true") {
        stack.pop();
        continue;
      }
      const style = getComputedStyle(node);
      if (style.display === "none" || style.visibility === "hidden" ||
          style.visibility === "collapse" || style.contentVisibility === "hidden") {
        stack.pop();
        continue;
      }
      if (node.isContentEditable) {
        editablePresent = true;
        output = "";
        break;
      }
      frame.separator = blockDisplays.has(style.display) || node.tagName === "BR"
        ? "\n" : (style.display === "table-cell" ? "\t" : "");
      if (frame.separator === "\n" || !separator) separator = frame.separator;
      frame.closedDetails = node.tagName === "DETAILS" && !node.hasAttribute("open");
      frame.child = node.firstChild;
    }
    if (frame.child) {
      const child = frame.child;
      frame.child = child.nextSibling;
      stack.push({ node: child, entered: false, details: frame.closedDetails ? frame : null });
    } else {
      if (frame.separator === "\n" || !separator) separator = frame.separator;
      stack.pop();
    }
  }
  // JS budgets count UTF-16 units; do not send half of an astral character.
  // Removing a trailing high surrogate leaves a genuine code-point prefix for
  // the host's Unicode boundary matcher without reading past either budget.
  if (sourceTruncated || outputTruncated || traversalTruncated) {
    const last = output.charCodeAt(output.length - 1);
    if (last >= 0xD800 && last <= 0xDBFF) output = output.slice(0, -1);
  }
  return {
    text: output,
    source_truncated: sourceTruncated,
    output_truncated: outputTruncated,
    traversal_truncated: traversalTruncated,
    editable_present: editablePresent,
  };
}
"""


@dataclass
class TabState:
    tab_id: str
    page: Any
    generation: int = 0
    references: Dict[str, SnapshotReference] = field(default_factory=dict)
    last_url: str = "about:blank"
    title: str = ""
    _typed_values: List[str] = field(default_factory=list, repr=False)

    def invalidate(self) -> None:
        for reference in self.references.values():
            reference.dispose()
        self.references.clear()
        if self.generation >= MAX_SNAPSHOT_GENERATION:
            raise BrowseError(
                "snapshot_generation_exhausted",
                "The portable snapshot-generation range is exhausted; close and relaunch the browser.",
            )
        self.generation += 1

    def close_references(self) -> None:
        for reference in self.references.values():
            reference.dispose()
        self.references.clear()

    def remember_typed_value(self, value: str) -> None:
        if not value:
            return
        encoded_candidates = [quote(value, safe=""), quote_plus(value)]
        candidates = [item for item in encoded_candidates if len(item.encode("utf-8")) <= 16_384]
        candidates.append(value)  # Keep the exact value newest so budget pruning retains it.
        for candidate in candidates:
            if not candidate:
                continue
            self._typed_values = [item for item in self._typed_values if item != candidate]
            self._typed_values.append(candidate)
        while (
            len(self._typed_values) > 24
            or sum(len(item.encode("utf-8")) for item in self._typed_values) > 24_576
        ):
            self._typed_values.pop(0)

    @property
    def has_typed_values(self) -> bool:
        return bool(self._typed_values)

    def redact(self, value: Any) -> str:
        text = str(value)
        for typed in sorted(self._typed_values, key=len, reverse=True):
            text = text.replace(typed, "[typed value withheld]")
        return text

    def redact_body(self, text: str, *, truncated: bool) -> str:
        """Mask all matching intervals, including possible prefixes at a cut.

        Only bounded raw DOM text enters here. KMP finds overlapping complete
        matches and the longest suffix/prefix in linear work per remembered
        value, using Python code points consistently (including astral text).
        Match both raw and normalized text, mapping normalized matches back to
        source intervals. Union BEFORE replacing anything: an earlier raw match
        could otherwise hide part of an overlapping normalized value. Markers
        are inserted only once, never fed back into the matching passes.
        """
        if not text or not self._typed_values:
            return text
        normalized_spans: List[Tuple[int, int]] = []
        normalized = _sanitize_multiline(text, len(text), source_spans=normalized_spans)
        views: List[Tuple[str, Optional[List[Tuple[int, int]]]]] = [(text, None)]
        if normalized != text:
            views.append((normalized, normalized_spans))
        # Normal HTML also collapses source line breaks into spaces. Treat
        # layout breaks conservatively for matching, without changing display.
        inline_normalized = normalized.replace("\n", " ")
        if inline_normalized != normalized:
            views.append((inline_normalized, normalized_spans))
        coverage = [0] * (len(text) + 1)
        for typed in self._typed_values:
            failure = [0] * len(typed)
            matched = 0
            for index in range(1, len(typed)):
                while matched and typed[index] != typed[matched]:
                    matched = failure[matched - 1]
                if typed[index] == typed[matched]:
                    matched += 1
                failure[index] = matched
            for observed, spans in views:
                matched = 0
                for index, character in enumerate(observed):
                    while matched and character != typed[matched]:
                        matched = failure[matched - 1]
                    if character == typed[matched]:
                        matched += 1
                    if matched == len(typed):
                        start, end = index + 1 - matched, index + 1
                        if spans is not None:
                            start, end = spans[start][0], spans[end - 1][1]
                        coverage[start] += 1
                        coverage[end] -= 1
                        matched = failure[matched - 1]
                if truncated and matched:
                    start, end = len(observed) - matched, len(observed)
                    if spans is not None:
                        start, end = spans[start][0], spans[end - 1][1]
                    coverage[start] += 1
                    coverage[end] -= 1
        output: List[str] = []
        active = 0
        for index, character in enumerate(text):
            previous = active
            active += coverage[index]
            if active:
                if not previous:
                    output.append("[typed value withheld]")
            else:
                output.append(character)
        return "".join(output)


@dataclass(frozen=True)
class SnapshotResult:
    tab_id: str
    generation: int
    text: str
    element_count: int
    truncated: bool


def snapshot_page(tab: TabState) -> SnapshotResult:
    """Replace refs atomically and return a bounded untrusted-content envelope."""

    tab.invalidate()
    generation = tab.generation
    page = tab.page
    new_references: Dict[str, SnapshotReference] = {}
    interactive_lines: List[str] = []
    truncated_elements = False
    sequence = 1

    try:
        for selector, role_hint in INTERACTIVE_GROUPS:
            locator = page.locator(selector)
            count = min(locator.count(), MAX_INTERACTIVE_ELEMENTS + 1)
            for index in range(count):
                if len(new_references) >= MAX_INTERACTIVE_ELEMENTS:
                    truncated_elements = True
                    break
                element = locator.nth(index)
                try:
                    if not element.is_visible():
                        continue
                    handle = element.element_handle()
                    if handle is None:
                        continue
                    metadata = inspect_target(
                        element,
                        role_hint=role_hint,
                        value_control_hint=selector.startswith("input")
                        or selector == "textarea",
                    )
                    if metadata.credential_like:
                        display_name = "manual credential field"
                    elif metadata.manual_value_possible:
                        display_name = "editable field (manual value withheld)"
                    else:
                        display_name = bounded_text(tab.redact(metadata.name), 72)
                    states = _element_states(element)
                    reference_name = f"e{sequence}"
                    sequence += 1
                    new_references[reference_name] = SnapshotReference(handle, metadata)
                    suffix = f" ({', '.join(states)})" if states else ""
                    interactive_lines.append(
                        f"[ref={reference_name}] role={metadata.role} name={display_name}{suffix}"
                    )
                except Exception:
                    continue
            if truncated_elements:
                break

        body_text = ""
        body_source_truncated = False
        body_output_truncated = False
        body_traversal_truncated = False
        editable_text_present = False
        try:
            editables = page.locator(
                'css=textarea, [contenteditable="true"], [contenteditable="plaintext-only"]'
            )
            editable_count = editables.count()
            # Do not assume uninspected fields after the check budget are safe.
            editable_text_present = editable_count > 100
            for index in range(min(editable_count, 100)):
                if editable_text_present or editables.nth(index).is_visible():
                    editable_text_present = True
                    break
        except Exception:
            editable_text_present = True
        if not editable_text_present:
            extracted = _extract_visible_body(page)
            editable_text_present = extracted["editable_present"]
            body_source_truncated = extracted["source_truncated"]
            body_output_truncated = extracted["output_truncated"]
            body_traversal_truncated = extracted["traversal_truncated"]
            # Redact complete values AND possible cut-boundary prefixes on the
            # host, before sanitizing or applying any subsequent character cap.
            if not editable_text_present:
                redacted = tab.redact_body(
                    extracted["text"],
                    truncated=(
                        body_source_truncated
                        or body_output_truncated
                        or body_traversal_truncated
                    ),
                )
                # Source is already bounded. Normalize before the host output
                # cap so discarded whitespace cannot cut a redaction marker.
                body_text = _sanitize_multiline(redacted, len(redacted))
                body_output_truncated |= len(body_text) > MAX_BODY_OUTPUT_CHARS
                body_text = body_text[:MAX_BODY_OUTPUT_CHARS]
        title = ""
        try:
            title = bounded_text(tab.redact(page.title()), 256)
        except Exception:
            pass
        tab.title = title
        tab.last_url = str(getattr(page, "url", "about:blank"))

        content_lines = [f"URL: {tab.redact(sanitize_url(tab.last_url))}"]
        if title:
            content_lines.append(f"Title: {title}")
        # Keep every returned ref visible even when long page text is truncated;
        # an undisclosed but actionable reference would be unsafe and confusing.
        if interactive_lines:
            content_lines.extend(["", "Interactive elements:", *interactive_lines])
        if truncated_elements:
            content_lines.append(
                f"[truncated: more than {MAX_INTERACTIVE_ELEMENTS} interactive elements]"
            )
        if editable_text_present:
            content_lines.append(
                "[visible body text omitted: editable content could contain manually entered values]"
            )
        else:
            body_notices = []
            if body_source_truncated:
                body_notices.append("[truncated: visible text source budget exceeded]")
            if body_output_truncated:
                body_notices.append("[truncated: visible text output budget exceeded]")
            if body_traversal_truncated:
                body_notices.append("[truncated: visible text traversal budget exceeded]")
            if body_notices:
                content_lines.extend(body_notices)
            if body_text:
                content_lines.extend(["", "Visible text:", body_text])
        untrusted = "\n".join(content_lines)
        prefix = (
            f"Browser snapshot for tab {tab.tab_id}; snapshot_generation={generation}.\n"
            f"{UNTRUSTED_BEGIN}\n"
        )
        suffix = f"\n{UNTRUSTED_END}"
        available = MAX_SNAPSHOT_CHARS - len(prefix) - len(suffix)
        truncated_text = len(untrusted) > available
        if truncated_text:
            notice = "\n[truncated: browser content exceeded the 20000-character snapshot bound]"
            untrusted = untrusted[: max(0, available - len(notice))] + notice
        text = prefix + untrusted + suffix
        tab.references = new_references
        return SnapshotResult(
            tab_id=tab.tab_id,
            generation=generation,
            text=text,
            element_count=len(new_references),
            truncated=(
                truncated_elements
                or truncated_text
                or body_source_truncated
                or body_output_truncated
                or body_traversal_truncated
            ),
        )
    except BaseException as error:
        for reference in new_references.values():
            reference.dispose()
        tab.references = {}
        if isinstance(error, BrowseError):
            raise
        raise BrowseError("snapshot_failed", "The bounded browser snapshot could not be created.") from error


def _extract_visible_body(page: Any) -> Mapping[str, Any]:
    """Read bounded raw text only; page code must never see the redaction set."""
    result = page.locator("body").evaluate(
        _VISIBLE_BODY_EXTRACTOR,
        {
            "max_source": MAX_BODY_SOURCE_CHARS,
            "max_output": MAX_BODY_OUTPUT_CHARS,
            "max_nodes": MAX_BODY_TRAVERSAL_NODES,
        },
        timeout=3000,
    )
    if (
        not isinstance(result, Mapping)
        or not isinstance(result.get("text"), str)
        or len(result["text"]) > MAX_BODY_OUTPUT_CHARS
        or any(
            not isinstance(result.get(flag), bool)
            for flag in (
                "source_truncated", "output_truncated", "traversal_truncated", "editable_present"
            )
        )
    ):
        raise BrowseError("snapshot_failed", "The bounded browser snapshot could not be created.")
    return result


def _element_states(element: Any) -> List[str]:
    states: List[str] = []
    try:
        if not element.is_enabled():
            states.append("disabled")
    except Exception:
        pass
    for attribute, label in (
        ("aria-checked", "checked"),
        ("aria-selected", "selected"),
        ("aria-expanded", "expanded"),
        ("checked", "checked"),
        ("selected", "selected"),
    ):
        try:
            value = element.get_attribute(attribute)
        except Exception:
            value = None
        if value is not None and value.lower() not in {"false", "0", "off"}:
            states.append(label)
    return list(dict.fromkeys(states))[:4]


def _sanitize_multiline(
    value: Any,
    limit: int,
    *,
    source_spans: Optional[List[Tuple[int, int]]] = None,
) -> str:
    """Normalize controls/whitespace, optionally recording each output's span.

    Span recording lets redaction preview this exact normalization and cover
    matching source intervals without ever matching generated masking markers.
    """
    output: List[str] = []
    pending_start: Optional[int] = None
    pending_break = False
    for index, character in enumerate(str(value)):
        if index >= limit:
            break
        if character not in {"\n", "\t"} and unicodedata.category(character).startswith("C"):
            character = " "
        if character.isspace():
            if pending_start is None:
                pending_start = index
            # Other splitlines separators are controls already replaced above.
            pending_break |= character in {"\n", "\u2028", "\u2029"}
            continue
        if output and pending_start is not None:
            output.append("\n" if pending_break else " ")
            if source_spans is not None:
                source_spans.append((pending_start, index))
        output.append(character)
        if source_spans is not None:
            source_spans.append((index, index + 1))
        pending_start = None
        pending_break = False
    return "".join(output)
