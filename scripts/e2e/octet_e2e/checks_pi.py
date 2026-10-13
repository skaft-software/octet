"""Pi-compat smoke checks: reviewed packages plus the shipped raw bundle."""

from __future__ import annotations

import re
import shutil
import time

from .fixtures import FixtureUnavailable, configure_pi_compat, install_hello_world
from .runner import CheckSkip, CheckSpec, Context
from .session import CheckFailure

#: The extension picker's own header; a picker row only exists inside that panel.
_PANEL_TITLE = "Manage extensions"


def _bridge(ctx: Context, env):
    try:
        return configure_pi_compat(env, ctx.repo_root, ctx.matrix_root)
    except FixtureUnavailable as unavailable:
        raise CheckSkip(str(unavailable)) from unavailable


def _close_expect(session, label: str) -> str:
    code = session.close()
    if code != 0:
        raise CheckFailure(f"{label}: Ctrl+D exited with {code}\n{session.tail()}")
    return f"{label}: clean exit 0"


def _open_extension_panel(session) -> str:
    session.submit("/extensions")
    session.wait_for_text(_PANEL_TITLE, timeout=20)
    time.sleep(0.3)
    return session.screen().text()


def _screen_snapshot(session, rows: int = 24) -> str:
    """Bounded screen excerpt for failure details (transcript + panel)."""
    lines = [line for line in session.screen().text().split("\n") if line.strip()]
    return "\n".join(lines[-rows:])


def _panel_row(panel_text: str, name: str) -> str | None:
    """Return the `/extensions` picker row for `name`, never a transcript line.

    Transcript notices such as `• [octet-pi-compat Info] …` also contain the
    bundle name, so only checkbox rows inside a live panel count: they carry
    `›`/`[x]`/`[ ]` followed by the exact bundle name. A frame without the
    panel header is stale and reports no row.
    """
    if _PANEL_TITLE not in panel_text:
        return None
    row_pattern = re.compile(r"^\s*(?:›\s*)?\[[ x]\]\s+" + re.escape(name) + r"\s")
    for row in panel_text.split("\n"):
        if row_pattern.match(row):
            return row.strip()
    return None


def check_compat_off(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.default_reply("R1-COMPAT-OFF-REPLY")
    env = ctx.environment("compat-off", provider)
    bridge = _bridge(ctx, env)  # the reviewed Pi material exists but stays disabled
    install_hello_world(env, ctx.repo_root)
    session = ctx.start(
        env,
        name="R1-compat-off",
        extension_dirs=[env.extensions],
        enable_extensions=["hello-world"],
    )
    panel = _open_extension_panel(session)
    row = _panel_row(panel, "octet-pi-compat")
    if row is not None and "running" in row:
        raise CheckFailure(
            f"octet-pi-compat ran without being enabled: {row!r}\n"
            f"screen snapshot:\n{_screen_snapshot(session)}"
        )
    session.key("escape")
    time.sleep(0.3)

    session.type_text("/ding")
    time.sleep(0.5)
    with_draft = session.screen().text()
    if "pi-ding" in with_draft:
        raise CheckFailure("a Pi extension command was registered with compat disabled")
    if "Configure and test pi-ding" in with_draft:
        raise CheckFailure("the Pi extension command's description appeared with compat disabled")
    session.key("escape")
    session.clear_composer("/ding")

    session.submit("R1-COMPAT-OFF-ASK")
    session.wait_for_text("R1-COMPAT-OFF-REPLY", timeout=30)
    evidence = [
        f"with the reviewed Pi extension dir present but not enabled: panel row {row!r}, no /ding command",
        f"bridge has {len(bridge['commands'])} commands and {len(bridge['tools'])} tools that stayed inactive",
    ]
    evidence.append(_close_expect(session, label="compat off"))
    return evidence


def check_compat_on(ctx: Context) -> list[str]:
    provider = ctx.provider()
    provider.default_reply("R1-COMPAT-ON-REPLY")
    env = ctx.environment("compat-on", provider)
    bridge = _bridge(ctx, env)
    missing = [command for command in ("ding", "powerline") if command not in bridge["commands"]]
    if missing:
        raise CheckFailure(f"reviewed Pi packages did not register expected commands: {missing}")
    memory_command = next(
        (command for command in bridge["commands"] if "memory" in command), None
    )
    if memory_command is None:
        raise CheckFailure("pi-hermes-memory registered no memory command")

    session = ctx.start(
        env,
        name="R1-compat-on",
        extension_dirs=[env.extensions],
        enable_extensions=["octet-pi-compat"],
    )
    transcript = session.transcript_text()
    if "[octet-pi-compat Error]" in transcript:
        error = next(
            (line for line in transcript.splitlines() if "[octet-pi-compat Error]" in line),
            "",
        )
        raise CheckFailure(
            f"compat startup reported an error: {error.strip()[:200]}\n"
            f"screen snapshot:\n{_screen_snapshot(session)}"
        )

    panel = _open_extension_panel(session)
    row = _panel_row(panel, "octet-pi-compat")
    if row is None or "running" not in row:
        raise CheckFailure(
            f"octet-pi-compat is not running with --enable-extension: {row!r}\n"
            f"screen snapshot:\n{_screen_snapshot(session)}"
        )

    session.key("escape")
    time.sleep(0.3)
    for command in ("ding", "powerline", memory_command):
        session.type_text(f"/{command}")
        session.wait_for(
            lambda screen: command in screen.text(), timeout=10, label=f"/{command} menu entry"
        )
        session.key("escape")
        session.clear_composer(f"/{command}")
        session.wait_for(
            lambda screen: f"/{command}" not in screen.text(), timeout=10, label=f"/{command} cleared"
        )

    session.submit("R1-COMPAT-ON-ASK")
    session.wait_for_text("R1-COMPAT-ON-REPLY", timeout=30)
    evidence = [
        f"octet-pi-compat running; {len(bridge['commands'])} commands and {len(bridge['tools'])} tools from 3 real packages",
        f"registered commands visible in the composer: /ding, /powerline, /{memory_command}",
        "a normal prompt round-trip worked with compat loaded",
    ]
    evidence.append(_close_expect(session, label="compat on"))
    return evidence


def check_compat_bundle(ctx: Context) -> list[str]:
    """The shipped raw bundle must start before any reviewed configure step.

    Loads the checkout's own `extensions/` as an explicit source: no matrix
    packages, no `configure.mjs --reviewed` capture. This is the acceptance
    path for the bundle's declared entrypoint (U22 fixes the adapter; U23 owns
    this check).
    """
    if shutil.which("node") is None:
        raise CheckSkip("node is required for the shipped octet-pi-compat bundle")
    bundle = ctx.repo_root / "extensions" / "octet-pi-compat"
    if not (bundle / "node_modules" / "jiti").exists():
        raise CheckSkip(
            f"adapter dependencies are not installed in {bundle}; run npm ci there once"
        )
    provider = ctx.provider()
    provider.default_reply("R1-COMPAT-BUNDLE-REPLY")
    env = ctx.environment("compat-bundle", provider)
    session = ctx.start(
        env,
        name="R1-compat-bundle",
        extension_dirs=[ctx.repo_root / "extensions"],
        enable_extensions=["octet-pi-compat"],
    )
    notices = [
        line.strip()
        for line in session.transcript_text().splitlines()
        if "octet-pi-compat did not start" in line or "[octet-pi-compat Error]" in line
    ]
    if notices:
        raise CheckFailure(
            f"the shipped octet-pi-compat bundle failed to start: {notices[0][:200]}\n"
            f"screen snapshot:\n{_screen_snapshot(session)}"
        )
    panel = _open_extension_panel(session)
    row = _panel_row(panel, "octet-pi-compat")
    if row is None or "running" not in row:
        raise CheckFailure(
            "the shipped octet-pi-compat bundle is not running with --enable-extension: "
            f"{row!r}\n"
            f"screen snapshot:\n{_screen_snapshot(session)}"
        )
    session.key("escape")
    time.sleep(0.3)
    session.submit("R1-COMPAT-BUNDLE-ASK")
    session.wait_for_text("R1-COMPAT-BUNDLE-REPLY", timeout=30)
    evidence = [
        "raw shipped bundle runs from extensions/ with zero reviewed factories",
        f"panel row: {row}",
    ]
    evidence.append(_close_expect(session, label="compat bundle"))
    return evidence


PI_CHECKS: list[CheckSpec] = [
    CheckSpec("compat-off", check_compat_off, "Pi compat disabled: nothing from the reviewed Pi material runs"),
    CheckSpec("compat-on", check_compat_on, "Pi compat enabled: 3 real packages load and register"),
    CheckSpec(
        "compat-bundle",
        check_compat_bundle,
        "Shipped raw bundle: octet-pi-compat starts from extensions/ with no reviewed factories",
    ),
]
