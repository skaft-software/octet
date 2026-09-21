"""Deterministic bounded-extraction and host-only redaction regressions."""

from __future__ import annotations

import itertools
import json
import shutil
import subprocess
import unicodedata
import unittest
from unittest.mock import patch
from urllib.parse import quote, quote_plus

from octet_browse.safety import BrowseError
from octet_browse.snapshot import (
    MAX_BODY_OUTPUT_CHARS,
    MAX_BODY_SOURCE_CHARS,
    MAX_BODY_TRAVERSAL_NODES,
    MAX_SNAPSHOT_CHARS,
    TabState,
    _VISIBLE_BODY_EXTRACTOR,
    _sanitize_multiline,
    snapshot_page,
)
from tests.helpers import FakeElement, FakeLocator, FakePage


# Run the production JS, not a Python reimplementation. Virtual billion-unit
# text and million-node trees allocate only what the extractor actually visits.
# These counters test work, not timing. Forbidden getters catch unbounded reads
# and child-array materialization. No browser, dependencies or network needed.
DOM_HARNESS = r"""
const { extractor, fixture, budgets } = JSON.parse(require('fs').readFileSync(0, 'utf8'));
const stats = { visited: 0, created: 0, read_units: 0, read_bytes: 0,
                reads: 0, max_read: 0, style_reads: 0, sibling_reads: 0 };
globalThis.Node = { ELEMENT_NODE: 1, TEXT_NODE: 3 };
globalThis.getComputedStyle = (node) => { stats.style_reads++; return node.style; };
function base(type) {
  stats.created++;
  return {
    get nodeType() { stats.visited++; return type; },
    firstChild: null,
    nextSibling: null,
    get childNodes() { throw Error('unbounded childNodes access'); },
    get innerText() { throw Error('unbounded innerText access'); },
    get textContent() { throw Error('unbounded textContent access'); },
    get data() { throw Error('unbounded CharacterData access'); },
    get nodeValue() { throw Error('unbounded nodeValue access'); },
    get value() { throw Error('form value access'); },
  };
}
function text(value, size = value.length) {
  const node = base(Node.TEXT_NODE);
  node.length = size;
  node.substringData = (offset, count) => {
    stats.reads++;
    stats.read_units += count;
    stats.read_bytes += 2 * count;
    stats.max_read = Math.max(stats.max_read, count);
    if (offset + count > size) throw Error('read past text length');
    return size === value.length ? value.slice(offset, offset + count) : value.repeat(count);
  };
  return node;
}
function element(tag, children = [], options = {}) {
  const node = base(Node.ELEMENT_NODE);
  node.tagName = tag.toUpperCase();
  const defaults = { BODY: 'block', DIV: 'block', P: 'block', PRE: 'block',
                     TD: 'table-cell', TR: 'table-row', DETAILS: 'block', SUMMARY: 'list-item' };
  node.style = { display: defaults[node.tagName] || 'inline', visibility: 'visible',
                 contentVisibility: 'visible', ...options.style };
  node.isContentEditable = !!options.editable;
  node.hasAttribute = (name) =>
    (name === 'hidden' && !!options.hidden) || (name === 'open' && !!options.open);
  node.getAttribute = (name) => {
    if (name === 'value') throw Error('form value attribute access');
    return name === 'aria-hidden' && options.ariaHidden ? 'true' : null;
  };
  node.firstChild = children[0] || null;
  for (let index = 0; index + 1 < children.length; index++) {
    children[index].nextSibling = children[index + 1];
  }
  return node;
}
function wide(size) {
  const body = element('body');
  function child(index) {
    const node = element('span');
    Object.defineProperty(node, 'nextSibling', { get() {
      stats.sibling_reads++;
      return index + 1 < size ? child(index + 1) : null;
    }});
    return node;
  }
  body.firstChild = child(0);
  return body;
}
function deep(size) {
  function child(depth) {
    const node = element(depth === 0 ? 'body' : 'div');
    Object.defineProperty(node, 'firstChild', { get() {
      return depth + 1 < size ? child(depth + 1) : null;
    }});
    return node;
  }
  return child(0);
}
const body = eval(fixture);
const result = eval('(' + extractor + ')')(body, budgets);
process.stdout.write(JSON.stringify({ result, stats }));
"""


def extracted(text: str = "", **flags: bool) -> dict:
    return {
        "text": text,
        "source_truncated": False,
        "output_truncated": False,
        "traversal_truncated": False,
        "editable_present": False,
        **flags,
    }


@unittest.skipUnless(shutil.which("node"), "Node unavailable; DOM extraction fixtures need Node")
class DOMExtractionTests(unittest.TestCase):
    def run_dom(self, fixture: str, **budgets: int) -> tuple[dict, dict]:
        completed = subprocess.run(
            [shutil.which("node"), "-e", DOM_HARNESS],
            input=json.dumps({
                "extractor": _VISIBLE_BODY_EXTRACTOR,
                "fixture": fixture,
                "budgets": {
                    "max_source": MAX_BODY_SOURCE_CHARS,
                    "max_output": MAX_BODY_OUTPUT_CHARS,
                    "max_nodes": MAX_BODY_TRAVERSAL_NODES,
                    **budgets,
                },
            }),
            text=True,
            capture_output=True,
            check=True,
            timeout=10,
        )
        observed = json.loads(completed.stdout)
        return observed["result"], observed["stats"]

    def test_enormous_single_text_node_reads_only_the_source_budget(self) -> None:
        result, stats = self.run_dom("element('body', [text('x', 1000000000)])")
        self.assertEqual(result["text"], "x" * MAX_BODY_SOURCE_CHARS)
        self.assertTrue(result["source_truncated"])
        self.assertTrue(result["output_truncated"])
        self.assertFalse(result["traversal_truncated"])
        self.assertEqual(stats["visited"], 2)
        self.assertEqual(stats["read_units"], MAX_BODY_SOURCE_CHARS)
        self.assertEqual(stats["read_bytes"], MAX_BODY_SOURCE_CHARS * 2)
        self.assertEqual(stats["reads"], (MAX_BODY_SOURCE_CHARS + 1023) // 1024)
        self.assertLessEqual(stats["max_read"], 1024)

    def test_million_wide_and_deep_trees_stop_at_the_node_budget(self) -> None:
        for shape in ("wide", "deep"):
            with self.subTest(shape=shape):
                result, stats = self.run_dom(f"{shape}(1000000)")
                self.assertEqual(result["text"], "")
                self.assertTrue(result["traversal_truncated"])
                self.assertEqual(stats["visited"], MAX_BODY_TRAVERSAL_NODES)
                self.assertLessEqual(stats["created"], MAX_BODY_TRAVERSAL_NODES + 2)
                self.assertLessEqual(stats["sibling_reads"], MAX_BODY_TRAVERSAL_NODES)
                self.assertEqual(stats["read_bytes"], 0)

    def test_hidden_scripts_styles_and_form_values_are_never_read(self) -> None:
        result, stats = self.run_dom("""element('body', [
          text('public'),
          ...['script', 'style', 'noscript', 'template', 'input', 'textarea',
              'select', 'option', 'datalist'].map(tag => element(tag, [text('SECRET')])),
          element('div', [text('SECRET')], {hidden: true}),
          element('div', [text('SECRET')], {ariaHidden: true}),
          element('div', [text('SECRET')], {style: {display: 'none'}}),
          element('div', [text('SECRET')], {style: {visibility: 'hidden'}}),
          element('div', [text('SECRET')], {style: {visibility: 'collapse'}}),
          element('div', [text('SECRET')], {style: {contentVisibility: 'hidden'}}),
          text(' content')
        ])""")
        self.assertEqual(result, extracted("public content"))
        self.assertEqual(stats["read_units"], len("public content"))

    def test_closed_details_only_reads_first_summary_but_open_content_is_visible(self) -> None:
        result, stats = self.run_dom("""element('body', [
          element('details', [text('SECRET'), element('summary', [text('Summary')]),
                              element('summary', [text('SECRET')]),
                              element('div', [text('SECRET')])]),
          element('details', [element('summary', [text('Open')]), text('Visible')], {open: true})
        ])""")
        self.assertEqual(_sanitize_multiline(result['text'], 100), "Summary\nOpen\nVisible")
        self.assertEqual(stats['read_units'], len("SummaryOpenVisible"))

    def test_normal_inline_block_break_table_and_unicode_text(self) -> None:
        result, _ = self.run_dom("""element('body', [
          element('p', [text('Hello '), element('span', [text('world 🌍')]),
                        element('br'), text('下一行')]),
          element('div', [text('after')]),
          element('div', [text(' inline')], {style: {display: 'inline'}}),
          element('span', [text('block')], {style: {display: 'block'}}),
          element('tr', [element('td', [text('one')]), element('td', [text('two')])])
        ])""")
        self.assertEqual(
            _sanitize_multiline(result["text"], MAX_BODY_OUTPUT_CHARS),
            "Hello world 🌍\n下一行\nafter\ninline\nblock\none two",
        )
        self.assertFalse(result["source_truncated"])
        self.assertFalse(result["traversal_truncated"])

    def test_output_budget_counts_separators_without_unbounded_reads(self) -> None:
        result, stats = self.run_dom(
            "element('body', [element('p', [text('ab')]), element('p', [text('cdefgh')])])",
            max_source=100,
            max_output=6,
        )
        self.assertEqual(result["text"], "ab\ncde")
        self.assertTrue(result["output_truncated"])
        self.assertFalse(result["source_truncated"])
        self.assertEqual(stats["read_units"], 5)

    def test_exact_budget_is_not_truncated_but_unread_whitespace_is(self) -> None:
        result, _ = self.run_dom("element('body', [text('abc')])", max_source=3, max_output=3)
        self.assertEqual(result, extracted("abc"))
        result, _ = self.run_dom("element('body', [text('abc ')])", max_source=3, max_output=20)
        self.assertEqual(result, extracted("abc", source_truncated=True))

    def test_cut_unicode_surrogate_pair_is_removed_before_host_prefix_matching(self) -> None:
        result, stats = self.run_dom("element('body', [text('safe 🔐private')])", max_source=6)
        self.assertEqual(result["text"], "safe ")
        self.assertTrue(result["source_truncated"])
        self.assertEqual(stats["read_units"], 6)
        result, _ = self.run_dom("element('body', [text('🦊ABC🔐private')])", max_source=6)
        tab = TabState("unicode", FakePage())
        tab.remember_typed_value("🦊ABC🔐private")
        self.assertEqual(tab.redact_body(result["text"], truncated=True), "[typed value withheld]")

    def test_traversal_cut_keeps_raw_prefix_not_an_artificial_trailing_newline(self) -> None:
        result, _ = self.run_dom(
            "element('body', [element('p', [text('private')]), element('p', [text('later')])])",
            max_nodes=3,
        )
        self.assertEqual(result["text"], "private")
        self.assertTrue(result["traversal_truncated"])

    def test_visible_editable_discards_body_without_reading_its_text(self) -> None:
        result, stats = self.run_dom(
            "element('body', [text('before'), element('div', [text('SECRET')], {editable: true})])"
        )
        self.assertEqual(result, extracted(editable_present=True))
        self.assertEqual(stats["read_units"], len("before"))

    def test_raw_whitespace_echo_is_masked_normally_and_at_every_dom_cut(self) -> None:
        fixture = "element('body', [text('secret   va'), text('lue')])"
        for budgets in ({}, {"max_source": 11}, {"max_output": 11}, {"max_nodes": 2}):
            with self.subTest(budgets=budgets):
                result, _ = self.run_dom(fixture, **budgets)
                self.assertIn("secret   va", result["text"])
                tab = TabState("normalized", FakePage())
                tab.remember_typed_value("secret value")
                with patch.object(FakeLocator, "evaluate", return_value=result):
                    snapshot = snapshot_page(tab)
                self.assertEqual(snapshot.truncated, bool(budgets))
                self.assertIn("Visible text:\n[typed value withheld]", snapshot.text)
                self.assertNotIn("secret", snapshot.text)

    def test_cut_boundary_and_full_overlapping_values_never_reach_snapshot(self) -> None:
        # Complete 'abcde' overlaps the partial 'cdefgh'; removing 'cde' then
        # exact-replacing would leave 'ab' visible. Check all three cut reasons.
        for budgets, fixture in (
            ({"max_source": 5}, "element('body', [text('abcdefgh')])"),
            ({"max_output": 5}, "element('body', [text('abcdefgh')])"),
            ({"max_nodes": 2}, "element('body', [text('abcde'), text('fgh')])"),
        ):
            with self.subTest(budgets=budgets):
                result, _ = self.run_dom(fixture, **budgets)
                tab = TabState("overlap", FakePage())
                for value in ("abcde", "cdefgh"):
                    tab.remember_typed_value(value)
                with patch.object(FakeLocator, "evaluate", return_value=result):
                    snapshot = snapshot_page(tab)
                self.assertTrue(snapshot.truncated)
                self.assertIn("Visible text:\n[typed value withheld]", snapshot.text)
                self.assertNotIn("abc", snapshot.text)


class BodyRedactionTests(unittest.TestCase):
    def test_partial_astral_and_encoded_variants_are_redacted(self) -> None:
        value = "🔐private phrase/?"
        tab = TabState("variants", FakePage())
        tab.remember_typed_value(value)
        for variant in (value, quote(value, safe=""), quote_plus(value)):
            for cut in range(1, len(variant) + 1):
                with self.subTest(variant=variant, cut=cut):
                    raw = "Public: " + variant[:cut]
                    self.assertEqual(tab.redact_body(raw, truncated=True), "Public: [typed value withheld]")
            self.assertEqual(tab.redact_body(variant, truncated=False), "[typed value withheld]")

    def test_nontruncated_prefix_is_not_masked_and_sanitization_follows_redaction(self) -> None:
        tab = TabState("plain", FakePage())
        tab.remember_typed_value("secrets")
        self.assertEqual(tab.redact_body("normal secret", truncated=False), "normal secret")
        tab.remember_typed_value("private\x00value")
        self.assertEqual(
            _sanitize_multiline(tab.redact_body("safe\x01 private\x00value", truncated=False), 100),
            "safe [typed value withheld]",
        )

    def test_whitespace_and_control_normalized_echoes_are_masked_before_clipping(self) -> None:
        for separator in ("   ", "\t", "\n", "\x00", "\r", "\u00a0", "\u200b", "\u2028"):
            for ending, truncated in (("value.", False), ("va", True), ("", True)):
                with self.subTest(separator=repr(separator), ending=ending):
                    raw = "Public: 🔐secret" + separator + ending
                    tab = TabState("normalized", FakePage())
                    tab.remember_typed_value("🔐secret value")
                    with patch.object(FakeLocator, "evaluate", return_value=extracted(
                        raw, source_truncated=truncated
                    )):
                        result = snapshot_page(tab)
                    expected = "Public: [typed value withheld]" + ("." if not truncated else "")
                    self.assertIn("Visible text:\n" + expected, result.text)
                    self.assertNotIn("secret", result.text)

    def test_raw_and_normalized_overlaps_are_unioned_before_inserting_markers(self) -> None:
        for raw, values, truncated in (
            ("secret   value", ("secret", "secret value"), False),
            ("secret   va", ("va", "secret value"), True),
            ("secret   value", ("cret   value", "secret value"), False),
        ):
            with self.subTest(raw=raw, values=values):
                tab = TabState("overlap", FakePage())
                for value in values:
                    tab.remember_typed_value(value)
                self.assertEqual(tab.redact_body(raw, truncated=truncated), "[typed value withheld]")

    def test_normalization_does_not_rematch_or_change_generated_markers(self) -> None:
        tab = TabState("markers", FakePage())
        for value in ("typed", "value", "withheld", "private\tphrase"):
            tab.remember_typed_value(value)
        raw = "typed   value\twithheld\nprivate\tphrase"
        redacted = tab.redact_body(raw, truncated=True)
        self.assertEqual(
            _sanitize_multiline(redacted, len(redacted)),
            "[typed value withheld] [typed value withheld] [typed value withheld]\n[typed value withheld]",
        )

    def test_sanitization_with_spans_preserves_the_existing_unicode_contract(self) -> None:
        alphabet = "A🔐 \t\n\r\u2028\u2029\u00a0\x00\u200b"
        for characters in itertools.product(alphabet, repeat=3):
            raw = "".join(characters)
            for limit in (1, len(raw)):
                # Independent expression of the previous sanitizer semantics.
                cleaned = "".join(
                    character if character in {"\n", "\t"} or not unicodedata.category(character).startswith("C")
                    else " " for character in raw[:limit]
                )
                lines = [" ".join(line.split()) for line in cleaned.splitlines()]
                expected = "\n".join(line for line in lines if line).strip()
                spans = []
                actual = _sanitize_multiline(raw, limit, source_spans=spans)
                self.assertEqual(actual, expected)
                self.assertEqual(len(spans), len(actual))
                self.assertTrue(all(0 <= start < end <= limit for start, end in spans))
                self.assertTrue(all(left[1] <= right[0] for left, right in zip(spans, spans[1:])))

    def test_overlapping_full_and_partial_coverage_matches_a_naive_oracle(self) -> None:
        alphabet = "ab"
        values = ["".join(item) for size in range(1, 4) for item in itertools.product(alphabet, repeat=size)]
        for first, second in itertools.combinations(values, 2):
            tab = TabState("oracle", FakePage())
            tab.remember_typed_value(first)
            tab.remember_typed_value(second)
            for text in ("aababa", "babbab", "ababab", "bbabaa"):
                for truncated in (False, True):
                    mask = [False] * len(text)
                    for typed in (first, second):
                        for start in range(len(text)):
                            tail = text[start:]
                            if tail.startswith(typed) or (truncated and typed.startswith(tail)):
                                for index in range(start, min(start + len(typed), len(text))):
                                    mask[index] = True
                    expected = "".join(
                        "[typed value withheld]" if covered and (index == 0 or not mask[index - 1])
                        else "" if covered else text[index]
                        for index, covered in enumerate(mask)
                    )
                    self.assertEqual(tab.redact_body(text, truncated=truncated), expected)

    def test_extraction_errors_fail_closed_and_dispose_new_refs(self) -> None:
        for result in (RuntimeError("sensitive page error"), {}, {"text": "unexpected"}):
            with self.subTest(result=type(result).__name__):
                page = FakePage()
                button = FakeElement("Go")
                page.selector_elements["button"] = [button]
                tab = TabState("failure", page)
                patch_arguments = {"side_effect": result} if isinstance(result, Exception) else {"return_value": result}
                with patch.object(FakeLocator, "evaluate", **patch_arguments):
                    with self.assertRaises(BrowseError) as failed:
                        snapshot_page(tab)
                self.assertEqual(failed.exception.code, "snapshot_failed")
                self.assertNotIn("sensitive", failed.exception.message)
                self.assertEqual(tab.references, {})
                self.assertTrue(button.disposed)

    def test_more_than_one_hundred_editables_omits_body_conservatively(self) -> None:
        page = FakePage(body="possible manually entered value")
        selector = 'css=textarea, [contenteditable="true"], [contenteditable="plaintext-only"]'
        page.selector_elements[selector] = [FakeElement(visible=False) for _ in range(100)] + [FakeElement()]
        result = snapshot_page(TabState("editables", page))
        self.assertNotIn(page.body, result.text)
        self.assertIn("editable content could contain manually entered values", result.text)
        self.assertEqual(page.body_evaluations, [])

    def test_host_redaction_expansion_sets_output_cut_without_source_cut(self) -> None:
        page = FakePage(body="x " * (MAX_BODY_OUTPUT_CHARS // 4))
        tab = TabState("expanded", page)
        tab.remember_typed_value("x")
        result = snapshot_page(tab)
        self.assertTrue(result.truncated)
        self.assertIn("output budget exceeded", result.text)
        self.assertNotIn("source budget exceeded", result.text)
        self.assertLessEqual(len(result.text), MAX_SNAPSHOT_CHARS)

    def test_large_whitespace_does_not_cut_a_redaction_marker_during_sanitization(self) -> None:
        page = FakePage(body=" " * (MAX_BODY_SOURCE_CHARS - 5) + "x")
        tab = TabState("whitespace", page)
        tab.remember_typed_value("x")
        result = snapshot_page(tab)
        self.assertFalse(result.truncated)
        self.assertIn("Visible text:\n[typed value withheld]", result.text)

    def test_final_snapshot_clip_does_not_expose_part_of_a_complete_value(self) -> None:
        value = "NEVER_EMIT_THIS" + "r" * 800
        page = FakePage(body="q" * 19_500 + value + "s" * 1000)
        tab = TabState("final_clip", page)
        tab.remember_typed_value(value)
        result = snapshot_page(tab)
        self.assertTrue(result.truncated)
        self.assertIn("[typed value withheld]", result.text)
        self.assertNotIn("NEVER_EMIT", result.text)
        self.assertLessEqual(len(result.text), MAX_SNAPSHOT_CHARS)


if __name__ == "__main__":
    unittest.main()
