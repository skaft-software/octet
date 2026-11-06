#!/usr/bin/env python3
"""Compare real Octet/Pi prompt and tool behavior using isolated synthetic setups.

No installs, user factories, ambient credentials or paid providers. A local
scripted provider captures the actual requests; its canned answer is not a grade.
Every failed leg is retained, and the command exits nonzero on any mismatch.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts/e2e"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from octet_e2e.provider import MockProvider, Reply  # noqa: E402
from fixture import write_provider_config  # noqa: E402

BASE = "PARITY_BASE"
PROMPT = "PARITY_USER"
CASES = {
    "startup": "let started = false; pi.on('session_start', () => { started = true; }); pi.on('before_agent_start', e => ({systemPrompt: e.systemPrompt + (started ? '\\nPARITY_STARTED' : '\\nMISSING_START')}));",
    "prompt-chain": "pi.on('before_agent_start', e => ({systemPrompt: e.systemPrompt + '\\nPARITY_FIRST'})); pi.on('before_agent_start', e => ({systemPrompt: e.systemPrompt + '\\nPARITY_SECOND'}));",
    "input-transform": "pi.on('input', e => ({action: 'transform', text: e.text + '\\nPARITY_TRANSFORMED'}));",
    "input-chain": "pi.on('input', e => ({action:'transform',text:e.text+'\\nPARITY_FIRST'})); pi.on('input', e => ({action:'transform',text:e.text+'\\nPARITY_SECOND'})); pi.on('before_agent_start', e => ({systemPrompt:e.systemPrompt+'\\n'+e.prompt}));",
    "input-handled": "pi.on('input', () => ({action:'handled'}));",
    # Pi has no prompt hook timeout. These prove Octet does not impose one either.
    "slow-input": "pi.on('input', async e => { await new Promise(r => setTimeout(r, 6500)); return {action:'transform', text: e.text + '\\nPARITY_SLOW'}; });",
    "slow-before-agent-start": "pi.on('before_agent_start', async e => { await new Promise(r => setTimeout(r, 6500)); return {systemPrompt: e.systemPrompt + '\\nPARITY_SLOW_SYSTEM'}; });",
    "tool-roundtrip": "pi.registerTool({name:'parity_echo', label:'Parity echo', description:'Synthetic side-effect-free echo', parameters:{type:'object',properties:{value:{type:'string'}},required:['value'],additionalProperties:false}, async execute(id,args){return {content:[{type:'text',text:'PARITY_TOOL_RESULT:'+args.value}]};}});",
    "large-fleet": "for(let i=0;i<117;i++) pi.registerTool({name:'parity_fleet_'+String(i).padStart(3,'0'),label:'Fleet',description:'Synthetic fleet '+ 'x'.repeat(1400),parameters:{type:'object',properties:{value:{type:'string'}},required:['value'],additionalProperties:false},async execute(){return {content:[{type:'text',text:'unused'}]};}});",
}


def text_content(content):
    if isinstance(content, str):
        return content
    return "\n".join(part.get("text", "") for part in content or [] if isinstance(part, dict) and part.get("type") == "text")


def observe(requests):
    """Keep semantic inputs/outputs, not timestamps, generated IDs or native prose."""
    result = []
    for request in requests:
        body = request.body
        messages = []
        for message in body.get("messages", []):
            row = {"role": message.get("role"), "text": text_content(message.get("content"))}
            if message.get("tool_calls"):
                row["tool_calls"] = [{"name": call["function"]["name"],
                                      "arguments": json.loads(call["function"]["arguments"])}
                                     for call in message["tool_calls"]]
            messages.append(row)
        tools = [entry.get("function", entry) for entry in body.get("tools", [])]
        # OpenAI function strictness defaults to false when omitted. Preserve
        # true (and every other field) so this does not hide real schema changes.
        tools = [{**tool, "strict": tool.get("strict", False)} for tool in tools
                 if tool.get("name", "").startswith("parity_")]
        result.append({"messages": messages, "tools": sorted(tools, key=lambda t: t["name"])})
    return result


def check(case, observations):
    errors = []
    if case == "input-handled":
        return ["handled input reached the provider"] if observations else []
    if not observations:
        return ["no provider request"]
    messages = observations[0]["messages"]
    system = "\n".join(m["text"] for m in messages if m["role"] == "system")
    user = "\n".join(m["text"] for m in messages if m["role"] == "user")
    expected_user = PROMPT + {"input-transform": "\nPARITY_TRANSFORMED",
                              "input-chain": "\nPARITY_FIRST\nPARITY_SECOND",
                              "slow-input": "\nPARITY_SLOW"}.get(case, "")
    expected_system = BASE + {"startup": "\nPARITY_STARTED", "prompt-chain": "\nPARITY_FIRST\nPARITY_SECOND",
                              "input-chain": "\n" + expected_user,
                              "slow-before-agent-start": "\nPARITY_SLOW_SYSTEM"}.get(case, "")
    if system != expected_system:
        errors.append(f"system prompt differs: {system!r}")
    if user != expected_user:
        errors.append(f"user prompt differs: {user!r}")
    if case == "large-fleet" and len(observations[0]["tools"]) != 117:
        errors.append("provider did not receive all 117 fleet tools")
    if case == "tool-roundtrip":
        if len(observations) != 2:
            errors.append(f"expected tool call + result request, got {len(observations)} requests")
        elif not any(m["role"] == "tool" and "PARITY_TOOL_RESULT:payload" in m["text"] for m in observations[1]["messages"]):
            errors.append("actual tool result missing from next provider request")
    return errors


def leg(target, case, root, args):
    work = root / target / case
    work.mkdir(parents=True, exist_ok=False)
    home, workspace, tmp = (work / name for name in ("home", "workspace", "tmp"))
    for path in (home, workspace, tmp):
        path.mkdir()
    factory = work / "fixture.mjs"
    # Native default context composition intentionally differs. Establish the
    # same extension-owned base before testing subsequent prompt contributions.
    factory.write_text("export default function(pi) {\npi.on('before_agent_start', () => ({systemPrompt:'PARITY_BASE'}));\n" + CASES[case] + "\n}\n")
    agent = home / ".pi/agent"
    agent.mkdir(parents=True)
    (agent / "settings.json").write_text(json.dumps({"extensions": [str(factory)], "compaction": {"enabled": False}}))
    provider = MockProvider()
    if case == "tool-roundtrip":
        provider.rule(lambda request: not any(m.get("role") == "tool" for m in request.messages),
                      lambda _: Reply(tool_call=("parity_echo", '{"value":"payload"}')))
    provider.default_reply("PARITY_ANSWER")
    write_provider_config(home, provider.port)
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "TMPDIR": str(tmp),
           "LANG": "C.UTF-8", "TERM": "dumb", "PI_OFFLINE": "1",
           "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0",
           "PI_CODING_AGENT_DIR": str(agent), "OCTET_PI_AGENT_DIR": str(agent),
           "NODE_COMPILE_CACHE": str(tmp / "node-cache"), "JITI_CACHE_DIR": str(tmp / "jiti"),
           "XDG_CACHE_HOME": str(tmp / "cache")}
    if target == "octet":
        command = [str(args.octet), "--offline", "--no-context-files", "--workspace", str(workspace),
                   "--session-dir", str(work / "sessions"), "--model", "custom/mock-1",
                   "--system-prompt", BASE, "--extension-dir", str(args.extensions_dir),
                   "--enable-extension", "octet-pi-compat", "--max-turns", "3", "--cache-warming", "off",
                   "-p" if args.octet_mode == "print" else "--plain", PROMPT]
    else:
        version = target.removeprefix("pi-")
        cli = args.pi_install / "releases" / version / "node_modules/@earendil-works/pi-coding-agent/dist/cli.js"
        command = [shutil.which("node"), str(cli), "--model", "mock/mock-1", "--system-prompt", BASE,
                   "--no-extensions", "--no-skills", "--extension", str(factory), "--no-session", "-p", PROMPT]
    started = time.monotonic()
    process = None
    timed_out = False
    try:
        process = subprocess.Popen(command, cwd=workspace, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGTERM)
            try:
                stdout, stderr = process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                stdout, stderr = process.communicate()
    finally:
        provider.close()
    (work / "stdout.log").write_bytes(stdout)
    (work / "stderr.log").write_bytes(stderr)
    (work / "requests.json").write_text(json.dumps([r.body for r in provider.requests], indent=2) + "\n")
    observations = observe(provider.requests)
    (work / "observations.json").write_text(json.dumps(observations, indent=2) + "\n")
    errors = check(case, observations)
    if case == "input-handled" and target == "octet":
        for session in (work / "sessions").rglob("*.jsonl"):
            records = [json.loads(line) for line in session.read_text().splitlines() if line]
            if any("User" in record.get("value", {}) for record in records):
                errors.append("handled input was persisted")
    if process.returncode != 0:
        errors.append(f"exit {process.returncode}")
    if timed_out:
        errors.append("timeout")
    row = {"target": target, "case": case, "seconds": round(time.monotonic() - started, 3),
           "requests": len(provider.requests), "errors": errors, "work": str(work)}
    print(json.dumps(row), flush=True)
    return row, observations


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--octet", type=Path, required=True)
    parser.add_argument("--octet-mode", choices=("print", "plain"), default="print")
    parser.add_argument("--extensions-dir", type=Path, default=REPO / "extensions")
    parser.add_argument("--pi-install", type=Path, required=True, help="read-only managed install directory, not the Pi agent home")
    parser.add_argument("--pi-versions", default="1.0.2,1.5.0")
    parser.add_argument("--cases", default=",".join(CASES))
    parser.add_argument("--reps", type=int, default=2)
    parser.add_argument("--timeout", type=float, default=45)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    cases = args.cases.split(",")
    if not 1 <= args.reps <= 20 or not 0 < args.timeout <= 120 or any(case not in CASES for case in cases):
        parser.error("use 1–20 repetitions, timeout <=120 seconds, and named cases")
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    args.octet = args.octet.resolve(strict=True)
    args.extensions_dir = args.extensions_dir.resolve(strict=True)
    args.pi_install = args.pi_install.resolve(strict=True)
    (args.out / "inputs.json").write_text(json.dumps({"octet": str(args.octet),
        "octet_sha256": hashlib.sha256(args.octet.read_bytes()).hexdigest(),
        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
        "source_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=REPO)),
        "cases": cases, "reps": args.reps, "octet_mode": args.octet_mode,
        "scope": "synthetic factories; real CLIs; local scripted provider; not full setup qualification"}, indent=2) + "\n")
    failures = 0
    targets = ["octet", *("pi-" + version for version in args.pi_versions.split(","))]
    for rep in range(args.reps):
        for case in cases:
            observations = {}
            for target in targets if rep % 2 == 0 else list(reversed(targets)):
                row, observations[target] = leg(target, case, args.out / f"rep-{rep}", args)
                failures += bool(row["errors"])
                with (args.out / "results.jsonl").open("a") as handle:
                    handle.write(json.dumps({"rep": rep, **row}) + "\n")
            for target in targets[1:]:
                same = observations["octet"] == observations[target]
                comparison = {"rep": rep, "case": case, "reference": target, "semantic_request_match": same}
                failures += not same
                with (args.out / "comparisons.jsonl").open("a") as handle:
                    handle.write(json.dumps(comparison) + "\n")
                print(json.dumps(comparison), flush=True)
    print(json.dumps({"failures": failures, "out": str(args.out)}), flush=True)
    return int(failures > 0)


if __name__ == "__main__":
    raise SystemExit(main())
