"""Core interactive checks ported from the U8 real-binary sweep."""

from __future__ import annotations

import statistics
import threading
import time

from .fixtures import install_hello_world
from .provider import contains, list_lines, numbered_lines
from .runner import CheckSpec, Context
from .session import CheckFailure, OctetSession
from .vt import Screen


# Light-variant palette cells the theme checks key on (U8's captures used a
# light terminal; the suite pins OCTET_COLOR_SCHEME=light).
CARDS_COMPOSER_BG = (239, 235, 228)  # Cards light composer_bg #efebe4
STILL_COMPOSER_BG = (236, 239, 235)  # Still light composer_bg #ecefeb
CARDS_MUTED_FG = (107, 114, 128)  # Cards light muted #6b7280
STILL_MUTED_FG = (102, 115, 107)  # Still light muted #66736b


def scan_bg(screen: Screen, rgb: tuple[int, int, int]) -> int:
    return sum(
        1
        for y in range(screen.rows)
        for x in range(screen.columns)
        if screen.cell(x, y)[1].bg == rgb
    )


def scan_fg(screen: Screen, rgb: tuple[int, int, int]) -> int:
    return sum(
        1
        for y in range(screen.rows)
        for x in range(screen.columns)
        if screen.cell(x, y)[1].fg == rgb
    )


def close_cleanly(session: OctetSession, *, label: str) -> str:
    code = session.close()
    if code != 0:
        raise CheckFailure(f"{label}: Ctrl+D exited with {code}\n{session.tail()}")
    return f"{label}: clean exit 0"


def check_startup(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.default_reply("R1-STARTUP-UNUSED")
    env = ctx.environment("startup", provider)
    session = ctx.start(env, name="R1-startup")
    screen = session.screen().text()
    if "octet v" not in screen:
        raise CheckFailure(f"startup splash missing\n{session.tail()}")
    if "Working" in screen:
        raise CheckFailure("idle startup already showed an active turn")
    evidence = [f"started {ctx.binary.name}, splash and idle composer present"]
    evidence.append(close_cleanly(session, label="startup"))
    return evidence


def check_resume_continue(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.reply_text(contains("R1-CONTINUE-ASK"), "R1-CONTINUE-REPLY")
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("resume-continue", provider)
    session = ctx.start(env, name="R1-continue-a")
    session.submit("R1-CONTINUE-ASK")
    session.wait_for_text("R1-CONTINUE-REPLY", timeout=30)
    session.wait_idle()
    close_cleanly(session, label="first session")

    resumed = ctx.start(env, name="R1-continue-b", set_name=False, args_extra=["--continue"])
    resumed.wait_for_text("R1-CONTINUE-REPLY", timeout=30)
    evidence = ["--continue restored the previous assistant reply"]
    evidence.append(close_cleanly(resumed, label="resumed session"))
    return evidence


def check_resume_picker(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.reply_text(contains("R1-PICKER-ASK"), "R1-PICKER-REPLY")
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("resume-picker", provider)
    session = ctx.start(env, name="R1-picker-a")
    session.submit("R1-PICKER-ASK")
    session.wait_for_text("R1-PICKER-REPLY", timeout=30)
    session.wait_idle()
    close_cleanly(session, label="first session")

    resumed = ctx.start(env, name="R1-picker-b", wait_ready=False, set_name=False, args_extra=["--resume"])
    resumed.wait_for_text("R1-picker-a", timeout=30)
    resumed.key("enter")
    resumed.wait_for_text("R1-PICKER-REPLY", timeout=30)
    evidence = ["bare --resume listed the named session and Enter restored it"]
    evidence.append(close_cleanly(resumed, label="picker resume"))
    return evidence


def check_scroll_long(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.reply_text(
        contains("R1-SCROLL-ASK"),
        numbered_lines("R1-LONG", 200),
        chunk_delay=0.02,
    )
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("scroll-long", provider)
    session = ctx.start(env, name="R1-scroll")
    session.submit("R1-SCROLL-ASK")
    session.wait_working()
    session.wait_for_text("R1-LONG-00", timeout=30)

    # Pinned viewport while the answer is still streaming: PageUp leaves the
    # tail and shows older numbered rows; PageDown returns to the live tail.
    newest = max(
        (int(token[8:11]) for token in session.screen().text().split() if token.startswith("R1-LONG-")),
        default=0,
    )
    session.key("page_up")
    session.wait_for(
        lambda screen: "R1-LONG-200" not in screen.text() and "R1-LONG-0" in screen.text(),
        timeout=10,
        label="a pinned viewport while streaming",
    )
    session.key("page_down")
    session.wait_for(
        lambda screen: any(
            token.startswith("R1-LONG-") and int(token[8:11]) >= newest
            for token in screen.text().split()
        ),
        timeout=15,
        label="the live tail after PageDown",
    )
    session.wait_idle(timeout=60)
    if "R1-LONG-200" not in session.screen().text():
        raise CheckFailure("the completed 200-line answer's tail was not on screen")

    pages = 0
    while "R1-LONG-001" not in session.screen().text() and pages < 20:
        session.key("page_up")
        pages += 1
        time.sleep(0.15)
    if "R1-LONG-001" not in session.screen().text():
        raise CheckFailure(f"scrolling back never reached R1-LONG-001 after {pages} PageUp presses")
    back_indicator = "rows back" in session.screen().text()

    downs = 0
    while "R1-LONG-200" not in session.screen().text() and downs < 25:
        session.key("page_down")
        downs += 1
        time.sleep(0.15)
    if "R1-LONG-200" not in session.screen().text():
        raise CheckFailure(f"PageDown never returned to the live tail after {downs} presses")

    evidence = [
        f"pinned viewport while streaming (newest was R1-LONG-{newest:03d}) and return to live with PageDown",
        f"idle scroll reached both ends: {pages} PageUp to R1-LONG-001, {downs} PageDown to R1-LONG-200",
        f"'rows back' indicator present while pinned: {back_indicator}",
    ]
    evidence.append(close_cleanly(session, label="scroll session"))
    return evidence


def check_selection_copy(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.reply_text(contains("R1-COPY-ASK"), list_lines("R1-CELL", 120), chunk_delay=0.004)
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("selection-copy", provider)
    session = ctx.start(env, name="R1-copy", mouse="app")
    session.submit("R1-COPY-ASK")
    session.wait_for_text("R1-CELL-120", timeout=40)
    session.wait_idle()
    close_cleanly(session, label="selection source session")

    # U8's repro shape: restart the saved session with app-owned mouse, page
    # into the pinned viewport, and select a visible middle row.
    session = ctx.start(env, name="R1-copy-resumed", set_name=False, args_extra=["--continue"])
    session.wait_for_text("R1-CELL-120", timeout=30)
    for _ in range(4):
        session.key("page_up")
        time.sleep(0.15)
    screen = session.screen()
    target_y = None
    for y, row in enumerate(screen.text().split("\n")):
        if "R1-CELL-0" in row:
            target_y = y
    if target_y is None:
        raise CheckFailure(f"no R1-CELL-0xx row visible after PageUp\n{session.tail(20)}")
    row = screen.text().split("\n")[target_y]
    # The pinned viewport paints a scrollbar glyph in the last column; it is
    # chrome, not transcript text, so select the row's content cells only.
    core = row.rstrip().rstrip("│┃|\xa6").rstrip()
    x0 = len(core) - len(core.lstrip())
    x1 = len(core) - 1
    expected = core[x0 : x1 + 1]
    selected_id = next(
        (token for token in expected.split() if token.startswith("R1-CELL-")), ""
    )

    baseline = len(session.osc52())
    # The pointer's focus is a cell boundary: releasing one cell past the last
    # content cell selects through the final character (a release on the text's
    # last cell would copy up to, but not including, it).
    session.drag(x0, target_y, x1 + 1, target_y)
    session.wait_for(lambda s: len(s.osc52) > baseline, timeout=10, label="clipboard OSC 52")
    copied = session.osc52()[-1].rstrip("\n")
    copied = copied.rstrip()
    if copied != expected.strip():
        raise CheckFailure(
            "mouse selection copied the wrong cells:\n"
            f"  selected screen row {target_y}: {expected!r}\n"
            f"  clipboard:                 {copied!r}"
        )
    if selected_id not in copied:
        raise CheckFailure(f"clipboard lost the selected line id {selected_id!r}: {copied!r}")
    return [
        f"resumed session: row {target_y} ({selected_id}) copied from the visible pinned viewport; "
        "clipboard text equals the selected row"
    ]


def check_copy_slash(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.reply_text(contains("R1-SLASHCOPY-ASK"), "R1-SLASHCOPY-REPLY-MARKER")
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("copy-slash", provider)
    session = ctx.start(env, name="R1-slashcopy")
    session.submit("R1-SLASHCOPY-ASK")
    session.wait_for_text("R1-SLASHCOPY-REPLY-MARKER", timeout=30)
    session.wait_idle()
    baseline = len(session.osc52())
    session.submit("/copy")
    session.wait_for(lambda s: len(s.osc52) > baseline, timeout=10, label="/copy clipboard")
    copied = session.osc52()[-1]
    if "R1-SLASHCOPY-REPLY-MARKER" not in copied:
        raise CheckFailure(f"/copy clipboard did not contain the last assistant message: {copied!r}")
    return ["/copy placed the last assistant message on the OSC 52 clipboard"]


def _theme_transition(
    session: OctetSession,
    expected_bg: tuple[int, int, int],
    previous_bg: tuple[int, int, int],
    *,
    label: str,
    timeout: float = 15.0,
) -> None:
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        screen = session.screen()
        if "invalid theme" in screen.text().lower():
            raise CheckFailure(f"{label}: octet rejected the theme\n{session.tail(12)}")
        if scan_bg(screen, expected_bg) and not scan_bg(screen, previous_bg):
            return
        last = screen.text()
        time.sleep(0.1)
    raise CheckFailure(
        f"{label}: expected composer background {expected_bg} without {previous_bg} within {timeout}s\n"
        f"{session.tail(12)}"
    )


def check_theme_direct(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("theme-direct", provider)
    session = ctx.start(env, name="R1-theme", theme="Cards")
    if not scan_bg(session.screen(), CARDS_COMPOSER_BG):
        raise CheckFailure("startup --theme Cards did not paint the Cards composer background")

    session.submit("/theme Still")
    _theme_transition(session, STILL_COMPOSER_BG, CARDS_COMPOSER_BG, label="/theme Still")
    still_muted = scan_fg(session.screen(), STILL_MUTED_FG) > 0

    session.submit("/theme Cards")
    _theme_transition(session, CARDS_COMPOSER_BG, STILL_COMPOSER_BG, label="/theme Cards")

    return [
        "startup --theme Cards applied; /theme Still and /theme Cards both applied directly",
        f"Still muted foreground present after direct selection: {still_muted}",
    ]


def check_theme_picker(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("theme-picker", provider)
    session = ctx.start(env, name="R1-theme-picker", theme="Cards")
    session.submit("/theme")
    session.wait_for_text("Still", timeout=15)
    session.type_text("Still")
    time.sleep(0.2)
    session.key("enter")
    _theme_transition(session, STILL_COMPOSER_BG, CARDS_COMPOSER_BG, label="theme picker Still")
    return ["theme picker filtered to Still and applied it immediately"]


def check_busy_slash(ctx: Context) -> list[str]:
    provider = ctx.provider()
    gate = threading.Event()
    provider.reply_text(contains("R1-BUSY-ASK"), numbered_lines("R1-BUSY", 60), gate=gate, chunk_delay=0.01)
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("busy-slash", provider)
    session = ctx.start(env, name="R1-busy")
    session.submit("R1-BUSY-ASK")
    session.wait_working()
    session.wait_for_text("R1-BUSY-001", timeout=30)

    session.submit(f"/model {env.model}")
    session.wait_for(lambda s: "queued" in s.text().lower(), timeout=15, label="queued /model notice")
    session.submit("/thinking off")
    session.wait_for(
        lambda s: s.text().lower().count("queued") >= 2, timeout=15, label="queued /thinking notice"
    )
    notices = [line for line in session.screen().text().split("\n") if "queued" in line.lower()]
    if not notices:
        raise CheckFailure("no queued notices visible while busy")
    gate.set()
    session.wait_idle(timeout=60)
    return [
        "busy /model and /thinking produced queued notices",
        f"queued row: {notices[-1].strip()[:100]}",
    ]


def check_busy_extension(ctx: Context) -> list[str]:
    provider = ctx.provider()
    gate = threading.Event()
    provider.reply_text(contains("R1-BUSYEXT-ASK"), numbered_lines("R1-BUSYEXT", 60), gate=gate, chunk_delay=0.01)
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("busy-extension", provider)
    install_hello_world(env, ctx.repo_root)
    session = ctx.start(
        env,
        name="R1-busy-ext",
        extension_dirs=[env.extensions],
        enable_extensions=["hello-world"],
    )
    session.submit("R1-BUSYEXT-ASK")
    session.wait_working()
    session.wait_for_text("R1-BUSYEXT-001", timeout=30)
    session.submit("/hello R1-BUSY")
    session.wait_for(lambda s: "queued" in s.text().lower(), timeout=15, label="queued extension command")
    gate.set()
    session.wait_for_text("Hello, R1-BUSY!", timeout=30)
    return ["extension command executed at the idle boundary after being queued while busy"]


def check_steer_enter(ctx: Context) -> list[str]:
    provider = ctx.provider()
    gate = threading.Event()
    provider.reply_text(contains("R1-STEER-MARKER"), "STEER-RECEIVED")
    provider.reply_text(contains("R1-STEER-ASK"), "R1-STEER-ACK", gate=gate)
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("steer", provider)
    session = ctx.start(env, name="R1-steer")
    session.submit("R1-STEER-ASK")
    session.wait_working()
    session.wait_for_text("R1-STEER-ACK", timeout=30)

    steering = "R1-STEER-MARKER: add STEER-RECEIVED"
    session.submit(steering)
    session.wait_for(lambda s: "R1-STEER-MARKER" in s.text(), timeout=10, label="steering input visible while busy")
    gate.set()
    session.wait_for_text("STEER-RECEIVED", timeout=30)
    session.wait_idle(timeout=60)
    requests = provider.request_matching("R1-STEER-MARKER")
    if not requests:
        raise CheckFailure("the steering message never reached the provider after the run")
    return [
        "Enter while busy queued steering; the model received it and the follow-up reply rendered",
    ]


def check_followup_ctrls(ctx: Context) -> list[str]:
    provider = ctx.provider()
    gate = threading.Event()
    provider.reply_text(contains("R1-FOLLOWUP-MARKER"), "FOLLOWUP-RECEIVED")
    provider.reply_text(contains("R1-FOLLOWUP-ASK"), "R1-FOLLOWUP-ACK", gate=gate)
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("followup", provider)
    session = ctx.start(env, name="R1-followup")
    session.submit("R1-FOLLOWUP-ASK")
    session.wait_working()
    session.wait_for_text("R1-FOLLOWUP-ACK", timeout=30)

    session.type_text("R1-FOLLOWUP-MARKER")
    session.key("ctrl_s")
    session.wait_for(
        lambda s: "R1-FOLLOWUP-MARKER" in s.text(), timeout=10, label="follow-up visible while busy"
    )
    gate.set()
    session.wait_for_text("FOLLOWUP-RECEIVED", timeout=30)
    session.wait_idle(timeout=60)
    requests = provider.request_matching("R1-FOLLOWUP-MARKER")
    if not requests:
        raise CheckFailure("the Ctrl+S follow-up never became its own user prompt")
    return ["Ctrl+S queued a follow-up that ran as a separate prompt after the turn settled"]


def check_typing_latency(ctx: Context) -> list[str]:
    provider = ctx.provider()
    gate = threading.Event()
    provider.reply_text(contains("R1-LAT-ASK"), numbered_lines("R1-LAT", 120), gate=gate, chunk_delay=0.02)
    provider.default_reply("R1-UNEXPECTED")
    env = ctx.environment("latency", provider)
    session = ctx.start(env, name="R1-latency")
    session.submit("R1-LAT-ASK")
    session.wait_working()
    session.wait_for_text("R1-LAT-001", timeout=30)

    samples: list[float] = []
    for index in range(15):
        marker = f"R1LAT{index:03d}"
        started = time.monotonic()
        session.type_text(marker)
        session.wait_for(lambda s: marker in s.text(), timeout=5, label=f"echo of {marker}")
        samples.append((time.monotonic() - started) * 1000)
        if "Working" not in session.screen().text():
            raise CheckFailure(f"the turn settled early; sample {index} was not taken while streaming")
        session.clear_composer(marker)
        session.wait_for(lambda s: marker not in s.text(), timeout=5, label=f"cleared {marker}")
    gate.set()
    session.wait_idle(timeout=60)

    maximum = max(samples)
    if maximum > 250:
        raise CheckFailure(f"typing echo stalled at {maximum:.1f} ms while streaming (limit 250 ms)")
    return [
        f"{len(samples)} streaming draft echoes, max {maximum:.1f} ms, "
        f"median {statistics.median(samples):.1f} ms (limit 250 ms)"
    ]


CHECKS: list[CheckSpec] = [
    CheckSpec("startup", check_startup, "binary starts the real TUI and exits cleanly"),
    CheckSpec("resume-continue", check_resume_continue, "--continue restores the previous session"),
    CheckSpec("resume-picker", check_resume_picker, "bare --resume picker lists and restores a session"),
    CheckSpec("scroll-long", check_scroll_long, "long transcript PageUp/PageDown pinning and tail return"),
    CheckSpec("selection-copy", check_selection_copy, "mouse selection copies the visible row"),
    CheckSpec("copy-slash", check_copy_slash, "/copy places the last assistant message on the clipboard"),
    CheckSpec("theme-direct", check_theme_direct, "/theme <built-in> applies directly and repaints"),
    CheckSpec("theme-picker", check_theme_picker, "the theme picker applies a built-in immediately"),
    CheckSpec("busy-slash", check_busy_slash, "busy /model and /thinking queue visibly"),
    CheckSpec("busy-extension", check_busy_extension, "extension command queues while busy and runs at idle"),
    CheckSpec("steer-enter", check_steer_enter, "Enter while busy steers the active run"),
    CheckSpec("followup-ctrls", check_followup_ctrls, "Ctrl+S while busy queues a follow-up prompt"),
    CheckSpec("typing-latency", check_typing_latency, "draft echo stays responsive while streaming"),
]
