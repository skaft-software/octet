"""Pi-compat on/off smoke checks with three real packages from ORDERS/matrix."""

from __future__ import annotations

import time

from .fixtures import FixtureUnavailable, configure_pi_compat, install_hello_world
from .runner import CheckSkip, CheckSpec, Context
from .session import CheckFailure


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
    session.wait_for_text("Manage extensions", timeout=20)
    time.sleep(0.3)
    return session.screen().text()


def _panel_row(panel_text: str, name: str) -> str | None:
    for row in panel_text.split("\n"):
        if name in row:
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
        raise CheckFailure(f"octet-pi-compat ran without being enabled: {row!r}")
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
        raise CheckFailure(f"compat startup reported an error: {error.strip()[:200]}")

    panel = _open_extension_panel(session)
    row = _panel_row(panel, "octet-pi-compat")
    if row is None or "running" not in row:
        raise CheckFailure(f"octet-pi-compat is not running with --enable-extension: {row!r}")

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


PI_CHECKS: list[CheckSpec] = [
    CheckSpec("compat-off", check_compat_off, "Pi compat disabled: nothing from the reviewed Pi material runs"),
    CheckSpec("compat-on", check_compat_on, "Pi compat enabled: 3 real packages load and register"),
]
