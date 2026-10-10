"""Self-checks for the suite's own terminal model and mock provider.

`run.py --self-test` exercises these before a release run; they are ordinary
Python assertions, not product tests, so a failure here means the harness (not
the binary) is broken.
"""

from __future__ import annotations

from .provider import MockProvider
from .vt import Screen


def _feed(text: str, columns: int = 20, rows: int = 4) -> Screen:
    screen = Screen(columns, rows)
    screen.feed(text.encode())
    return screen


def _panel_frame(lines: list[str], columns: int = 120, rows: int = 40) -> str:
    """Render lines through the suite's VT model, as `screen().text()` sees them."""
    return _feed("\r\n".join(lines), columns=columns, rows=rows).text()


def selftest() -> bool:
    checks = [
        test_plain_text_and_wrap,
        test_cursor_and_erase,
        test_sgr_truecolor,
        test_scrollback_and_region,
        test_osc52_and_title,
        test_alternate_screen,
        test_panel_row_selects_checkbox_rows,
        test_provider_records_requests,
    ]
    failed = 0
    for check in checks:
        try:
            check()
            print(f"ok   {check.__name__}")
        except AssertionError as error:
            failed += 1
            print(f"FAIL {check.__name__}: {error}")
    return failed == 0


def test_plain_text_and_wrap() -> None:
    # The PTY's ONLCR turns the product's newlines into CRLF, as a terminal
    # sees them; a bare LF only moves down, which is asserted separately.
    screen = _feed("hello\r\nworld")
    assert screen.row(0) == "hello", screen.text()
    assert screen.row(1) == "world", screen.text()
    screen = _feed("abcdefghijklmnopqrstuv")
    assert screen.row(0) == "abcdefghijklmnopqrst", screen.text()
    assert screen.row(1) == "uv", screen.text()
    screen = _feed("ab\ncd")
    assert screen.row(1) == "  cd", repr(screen.text())


def test_cursor_and_erase() -> None:
    screen = _feed("first\r\nsecond\r\nthird")
    screen.feed(b"\x1b[1;1H\x1b[K")
    assert screen.row(0) == "", screen.text()
    assert screen.row(1) == "second", screen.text()
    screen.feed(b"\x1b[3;2H")
    assert (screen.x, screen.y) == (1, 2), (screen.x, screen.y)
    screen.feed(b"\x1b[2A")
    assert screen.y == 0, screen.y
    screen.feed(b"\x1b[0J")
    assert screen.row(0) == "", screen.text()
    assert screen.row(2) == "", screen.text()


def test_sgr_truecolor() -> None:
    screen = _feed("\x1b[38;2;102;115;107mstill\x1b[0m ok")
    cell, style = screen.cell(0, 0)
    assert cell == "s" and style.fg == (102, 115, 107), style
    _, plain = screen.cell(6, 0)
    assert plain.fg is None, plain


def test_scrollback_and_region() -> None:
    screen = _feed("one\r\ntwo\r\nthree\r\nfour\r\nfive", columns=10, rows=3)
    assert screen.scrollback == ["one", "two"], screen.scrollback
    assert [screen.row(y) for y in range(3)] == ["three", "four", "five"]
    screen = _feed("a\r\nb\r\nc\r\nd\x1b[1;2r\x1b[2;1H\r\nx", columns=10, rows=4)
    assert screen.scroll_top == 0 and screen.scroll_bottom == 1, (screen.scroll_top, screen.scroll_bottom)
    assert [screen.row(y) for y in range(4)] == ["b", "x", "c", "d"], screen.text()


def test_osc52_and_title() -> None:
    screen = _feed("\x1b]52;c;SGVsbG8=\x07\x1b]2;my title\x07tail")
    assert screen.osc52 == ["Hello"], screen.osc52
    assert screen.title == "my title", screen.title
    assert screen.row(0) == "tail", screen.text()


def test_alternate_screen() -> None:
    screen = Screen(20, 4)
    screen.feed(b"main\x1b[?1049h")
    screen.feed(b"alt")
    assert screen.row(0) == "alt", screen.text()
    screen.feed(b"\x1b[?1049l")
    assert screen.row(0) == "main", screen.text()


def test_provider_records_requests() -> None:
    provider = MockProvider()
    try:
        import json
        import urllib.request

        provider.default_reply("PROVIDER-SELFTEST")
        request = urllib.request.Request(
            f"http://127.0.0.1:{provider.port}/v1/chat/completions",
            data=json.dumps({"messages": [{"role": "user", "content": "ping"}], "stream": False}).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=10) as response:
            body = json.loads(response.read())
        assert body["choices"][0]["message"]["content"] == "PROVIDER-SELFTEST", body
        assert provider.wait_for_request("ping", timeout=2)
    finally:
        provider.close()


def test_panel_row_selects_checkbox_rows() -> None:
    """`/extensions` lookup must return the picker row, never a transcript notice.

    Recorded regression (RC run 3, `e2e-scratch/logs/18-R1-compat-on.ansi`,
    panel-open frame at byte 10752): an extension Info notice

        • [octet-pi-compat Info] 🧠 Session backfill complete: 0 indexed, 0 skipped, 0 messages.

    sits above the picker row

        › [x] octet-pi-compat     running · Host authority: granted (--extension-dir) · 0.1.0 · …

    and the first-match lookup returned the notice, so compat-on failed while
    the extension was running.
    """
    from .checks_pi import _panel_row

    notice = "• [octet-pi-compat Info] 🧠 Session backfill complete: 0 indexed, 0 skipped, 0 messages."
    running = (
        "› [x] octet-pi-compat     running · Host authority: granted (--extension-dir)"
        " · 0.1.0 · from --extension-dir; enable or…"
    )
    stopped = (
        "  [ ] octet-pi-compat     stopped · Host authority: granted (--extension-dir)"
        " · 0.1.0 · from --extension-dir; enable or…"
    )
    footer = "enter select · ↑↓ navigate · esc close"

    frame = [notice, "Manage extensions", "Filter  type to filter", running, footer]
    row = _panel_row(_panel_frame(frame), "octet-pi-compat")
    assert row is not None and "running" in row, f"notice beat the panel row: {row!r}"

    # A stopped bundle row is still the row the compat-off check must see.
    row = _panel_row(_panel_frame([notice, "Manage extensions", stopped, footer]), "octet-pi-compat")
    assert row is not None and "stopped" in row and "running" not in row, repr(row)

    # Exact-name boundaries: a longer name that merely contains the target must
    # not satisfy the lookup, neither as a row nor as a notice.
    longer = _panel_frame(
        [
            "• [octet-pi-compat-extra Info] ready",
            "Manage extensions",
            "› [x] octet-pi-compat-extra  running · Host authority: granted (--extension-dir) · 0.1.0",
            footer,
        ]
    )
    assert _panel_row(longer, "octet-pi-compat") is None, repr(_panel_row(longer, "octet-pi-compat"))

    # Genuine panel-absent cases: a notice alone, and a stale checkbox row with
    # no panel header, must both report no bundle row.
    assert _panel_row(_panel_frame([notice, "hello from the transcript"]), "octet-pi-compat") is None
    stale = _panel_frame(["› [x] octet-pi-compat  running · stale frame with no panel header"])
    assert _panel_row(stale, "octet-pi-compat") is None, repr(_panel_row(stale, "octet-pi-compat"))


if __name__ == "__main__":
    raise SystemExit(0 if selftest() else 1)
