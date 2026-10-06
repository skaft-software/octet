"""Real API 0.4 stdio tests of the Rust extension. No octet host is needed.

Python is only the test broker: every case starts the actual Rust executable
(with PATH empty), speaks the real feature-negotiated wire and drives real
guest scripts. The extension is binary-only in these tests; the runner it owns
is the same executable with a fresh isolated VM per script.
"""
import asyncio
import base64
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import signal
import tempfile
import unittest

from support import (
    FRAME_BYTES,
    OUTPUT_BYTES,
    dumps,
    loads,
    offer,
    runner_binary,
)

RUNNER = Path(os.environ.get("CODEMODE_RUNNER", runner_binary())).resolve()
PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a0n8AAAAASUVORK5CYII="
TOOLS = [
    {"name": "first", "description": "Read numbers", "parameters": {"type": "object"}, "output_schema": {"type": "object"}},
    {"name": "second", "description": "Double a number", "parameters": {"type": "object"}, "output_schema": {"type": "integer"}},
    {"name": "plain", "description": "Text result", "parameters": {"type": "object"}},
]
HOLD = object()


def context():
    return {"tools": copy.deepcopy(TOOLS), "store": {"previous": 5}, "limits": {"timeout_ms": 30000, "max_calls": 256}}


def text(result):
    return "\n".join(part["text"] for part in result["content"] if part["type"] == "text")


async def process_table():
    # This observer belongs to the Python test broker, not the extension tree.
    ps = shutil.which("ps")
    if ps is None:
        raise AssertionError("The lifecycle tests require the POSIX ps observer")
    observer = await asyncio.create_subprocess_exec(
        ps, "-ww", "-axo", "pid=,ppid=,command=", stdout=asyncio.subprocess.PIPE)
    output, _ = await asyncio.wait_for(observer.communicate(), 3)
    if observer.returncode != 0:
        raise AssertionError("Could not observe the real extension process tree")
    return {int(pid): (int(parent), command) for pid, parent, command in
            (row.split(None, 2) for row in output.decode().splitlines())}


def host_file(root, value, name="host.json"):
    data = dumps(value).encode()
    path = Path(root) / name
    with path.open("xb") as stream:
        stream.write(data)
    path.chmod(0o600)
    return {"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


class Broker:
    def __init__(self, engine, root, binary, handler=None, command=None, cwd=None, env=None,
                 offer_builder=None):
        self.engine, self.root, self.binary, self.handler = engine, root, binary, handler
        # An empty PATH is the test invariant: nothing may need Node or Python.
        self.env = env or {}
        # Benchmarks drive the previous Node/Pi runtime on the offer it accepts.
        self.offer_builder = offer_builder or (lambda optional: offer(optional))
        # Benchmarks override the launch command (for example to drive the
        # previous Node/Pi runtime); tests always run the Rust executable.
        self.command = command or [str(binary), "serve", "--engine", engine]
        self.cwd = cwd or binary.parent
        self.process = None
        self.messages, self.calls, self.commits, self.artifacts = [], [], [], []
        self.context = context()
        self.condition = asyncio.Condition()
        self.lock = asyncio.Lock()
        self.id = 1
        self.failure = None
        self.closed = False

    async def start(self, artifacts=True, initialize=True):
        environment = {"PATH": "", "OCTET_EXTENSION_SCRATCH": str(self.root), **self.env}
        self.process = await asyncio.create_subprocess_exec(
            *self.command, cwd=self.cwd,
            env=environment, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, limit=FRAME_BYTES + 1)
        self.reader = asyncio.create_task(self.read())
        if initialize:
            optional = ["tool_composition_v1", "request_progress"] + (["artifacts"] if artifacts else [])
            await self.send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                             "params": self.offer_builder(optional)})
            initialized = await self.wait(lambda m: m.get("id") == 1)
            if "result" not in initialized:
                raise AssertionError(initialized)
        return self

    async def send(self, value):
        async with self.lock:
            self.process.stdin.write((dumps(value) + "\n").encode())
            await self.process.stdin.drain()

    async def respond(self, message, result):
        await self.send({"jsonrpc": "2.0", "id": message["id"], "result": result})

    async def deny(self, message, code=-32002, reason="Host denied nested effect"):
        await self.send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": code, "message": reason}})

    async def read(self):
        try:
            while raw := await self.process.stdout.readline():
                if len(raw) > FRAME_BYTES + 1 or not raw.endswith(b"\n"):
                    raise AssertionError("Unbounded/truncated adapter frame")
                message = loads(raw.decode())
                async with self.condition:
                    self.messages.append(message)
                    self.condition.notify_all()
                method = message.get("method")
                if "id" not in message or method is None:
                    continue
                if method == "composition/call":
                    self.calls.append(message)
                elif method == "composition/store":
                    self.commits.append(message)
                elif method == "artifact/publish":
                    self.artifacts.append(message)
                response = self.handler(message) if self.handler else None
                if response is HOLD:
                    continue
                if response is not None:
                    await self.respond(message, response)
                elif method == "composition/context":
                    await self.respond(message, self.context)
                elif method == "composition/call":
                    fields = message["params"]
                    value = {"numbers": [1, 2, 3], "unicode": "🌱"} if fields["name"] == "first" else fields["arguments"].get("value", 0) * 2 if fields["name"] == "second" else '{"not":"parsed"}'
                    await self.respond(message, {"value": value})
                elif method == "composition/store":
                    self.context["store"].update(message["params"]["set"])
                    for key in message["params"]["delete"]:
                        self.context["store"].pop(key, None)
                    await self.respond(message, {})
                elif method == "artifact/publish":
                    fields = message["params"]
                    data = base64.b64decode(fields["data"]["data"]) if "data" in fields else (Path(self.root) / fields["path"]).read_bytes()
                    if len(data) != fields["size"] or hashlib.sha256(data).hexdigest() != fields["sha256"]:
                        raise AssertionError("Invalid artifact size/digest")
                    await self.respond(message, {"artifact_id": "artifact:test"})
                else:
                    raise AssertionError("Unexpected reverse method: " + str(method))
        except Exception as error:  # noqa: BLE001 - surfaced by wait()
            self.failure = error
        finally:
            async with self.condition:
                self.condition.notify_all()

    async def wait(self, predicate, timeout=10):
        async with asyncio.timeout(timeout):
            async with self.condition:
                while True:
                    for message in self.messages:
                        if predicate(message):
                            return message
                    if self.failure:
                        raise self.failure
                    if self.reader.done():
                        raise AssertionError("Adapter stopped: " + (await self.process.stderr.read()).decode())
                    await self.condition.wait()

    async def request(self, method, fields):
        self.id += 1
        await self.send({"jsonrpc": "2.0", "id": self.id, "method": method, "params": fields})
        return self.id

    async def start_code(self, code):
        return await self.request("tool/call", {"name": "codemode", "arguments": {"code": code}, "context": {}})

    async def run(self, code):
        id_ = await self.start_code(code)
        message = await self.wait(lambda m: m.get("id") == id_)
        if "error" in message:
            raise AssertionError(message)
        return message["result"]

    async def cancel(self, id_):
        await self.send({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": id_, "reason": "test"}})

    async def close(self):
        if self.closed or self.process is None:
            return
        self.closed = True
        try:
            if self.process.returncode is None:
                id_ = await self.request("shutdown", {})
                await self.wait(lambda m: m.get("id") == id_, timeout=5)
                await asyncio.wait_for(self.process.wait(), 5)
        finally:
            if self.process.returncode is None:
                self.process.kill()
            await self.process.wait()
            self.reader.cancel()
            await asyncio.gather(self.reader, return_exceptions=True)
            self.process.stdin.close()
            await self.process.stderr.read()


class EngineCases:
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="codemode-test-")
        self.addCleanup(self.temp.cleanup)
        standalone = Path(self.temp.name).resolve() / "standalone"
        standalone.mkdir()
        self.binary = Path(shutil.copy2(RUNNER, standalone / "octet-codemode"))
        self.assertEqual(list(standalone.iterdir()), [self.binary])
        self.binary_hash = hashlib.sha256(self.binary.read_bytes()).digest()
        self.scratch = Path(self.temp.name).resolve() / "scratch"
        self.scratch.mkdir(mode=0o700)
        self.brokers = []

    async def asyncTearDown(self):
        for broker in self.brokers:
            await broker.close()

    async def broker(self, handler=None, artifacts=True, initialize=True):
        broker = Broker(self.engine, self.scratch, self.binary, handler)
        self.brokers.append(broker)
        return await broker.start(artifacts, initialize)

    async def assert_runner(self, broker):
        """Exactly one warm Rust runner, using the admitted private image."""
        table = await process_table()
        self.assertEqual(table[broker.process.pid][1], f"{self.binary} serve --engine {self.engine}")
        children = [pid for pid, (parent, _) in table.items() if parent == broker.process.pid]
        self.assertEqual(len(children), 1, "Exactly one warm Rust runner must be running")
        child = children[0]
        command, engine = table[child][1].rsplit(" ", 1)
        self.assertEqual(engine, self.engine)
        image = Path(command)
        self.assertEqual(hashlib.sha256(image.read_bytes()).digest(), self.binary_hash,
                         "Runner must use the admitted executable image")
        self.assertEqual(image.stat().st_mode & 0o077, 0)
        self.assertTrue(image.is_relative_to(self.scratch))
        descendants = [pid for pid, (parent, _) in table.items() if parent == child]
        self.assertEqual(descendants, [], "The Rust runner must not launch another executable")
        for command in (table[broker.process.pid][1], table[child][1]):
            lowered = command.lower()
            self.assertNotIn("node", lowered)
            self.assertNotIn("python", lowered)
        return child

    async def test_initialization_required_and_negotiation_limits(self):
        b = await self.broker(initialize=False)
        early = await b.start_code('return 1;')
        self.assertEqual((await b.wait(lambda m: m.get("id") == early))["error"]["code"], -32002)
        init = offer()
        for bad in ({**init, "api_version": "0.3"},
                    {**init, "flag_values": [{"name": "codemode-mode", "value": "off"}]},
                    {**init, "protocol": {**init["protocol"], "optional_features": []}},
                    {**init, "protocol": {**init["protocol"], "limits": {"max_concurrent_requests": 3}}},
                    {**init, "protocol": {**init["protocol"], "required_features": ["request_cancellation", "bulk_objects_v1"]}}):
            request = await b.request("initialize", bad)
            self.assertEqual((await b.wait(lambda m: m.get("id") == request))["error"]["code"], -32602)
        self.assertFalse(any(m.get("method", "").startswith("composition/") for m in b.messages))
        self.assertFalse(any(parent == b.process.pid for parent, _ in (await process_table()).values()))
        init["flag_values"] = [{"name": "codemode-mode", "value": "only"},
                               {"name": "codemode-inline-budget", "value": 16000}]
        request = await b.request("initialize", init)
        result = (await b.wait(lambda m: m.get("id") == request))["result"]
        self.assertEqual(result["tools"][0]["composition"], {"mode": "only", "inline_budget": 16000})
        self.assertEqual(result["protocol"]["limits"]["max_concurrent_requests"], 8)
        self.assertNotIn("bulk_objects_v1", result["protocol"]["features"])
        self.assertNotIn("resource_refs_v1", result["protocol"]["features"])
        duplicate = await b.request("initialize", init)
        self.assertEqual((await b.wait(lambda m: m.get("id") == duplicate))["error"]["code"], -32602)
        self.assertFalse((await b.run('return "ready";'))["is_error"])

    async def test_chain_structured_results_and_plain_text_without_node(self):
        b = await self.broker()
        result = await b.run('const a=await tools.first({}); const n=await tools.second({value:a.numbers.length}); text(a.unicode);return [n,typeof await tools.plain({})];')
        self.assertFalse(result["is_error"], text(result))
        self.assertIn('[6,"string"]', text(result))
        self.assertIn("🌱", text(result))
        self.assertEqual([m["params"]["name"] for m in b.calls], ["first", "second", "plain"])
        self.assertTrue(all(m["params"]["parent_request_id"] == 2 for m in b.calls))
        isolated = await b.run('return [typeof process,typeof require,typeof fetch,typeof setTimeout,typeof Buffer,typeof models,typeof __emit].join(",");')
        self.assertIn(",".join(["undefined"] * 7), text(isolated))

    async def test_store_success_failure_and_branch_snapshot(self):
        b = await self.broker()
        result = await b.run('store("answer",load("previous")+1);store("previous",undefined);return load("answer");')
        self.assertFalse(result["is_error"], text(result))
        self.assertEqual(b.commits[0]["params"]["set"], {"answer": 6})
        self.assertEqual(b.commits[0]["params"]["delete"], ["previous"])
        result = await b.run('text("partial");store("answer",99);throw new Error("oops");')
        self.assertTrue(result["is_error"])
        self.assertIn("partial", text(result))
        self.assertIn("oops", text(result))
        self.assertEqual(result["metadata"]["error_kind"], "script")
        self.assertEqual(len(b.commits), 1)
        self.assertTrue(text(await b.run('return load("answer");')).endswith("\n6"))
        b.context["store"] = {"answer": 40}  # Host supplies another branch; no local cached ancestry.
        self.assertTrue(text(await b.run('return load("answer");')).endswith("\n40"))

    async def test_warm_runner_is_reused_and_scripts_stay_isolated(self):
        b = await self.broker()
        first = await b.run('globalThis.__leaked=1;Object.prototype.polluted=1;store("answer",1);return "first";')
        self.assertFalse(first["is_error"], text(first))
        runner = await self.assert_runner(b)
        second = await b.run('return [typeof globalThis.__leaked,Object.prototype.polluted===undefined,typeof load("answer")];')
        self.assertFalse(second["is_error"], text(second))
        self.assertIn('["undefined",true,"number"]', text(second))
        self.assertEqual(await self.assert_runner(b), runner, "The warm runner must be reused")
        third = await b.run('throw new Error("failed");')
        self.assertTrue(third["is_error"])
        fourth = await b.run('return "after failure";')
        self.assertFalse(fourth["is_error"], text(fourth))
        self.assertEqual(await self.assert_runner(b), runner)

    async def test_rejected_host_effects_and_invalid_guest_arguments(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/call" else None)
        id_ = await b.start_code('return (await Promise.allSettled([tools.first({}),tools.second({})])).map(x=>x.status);')
        await b.wait(lambda _: len(b.calls) == 2)
        for call in b.calls:
            await b.deny(call)
        result = (await b.wait(lambda m: m.get("id") == id_))["result"]
        self.assertFalse(result["is_error"], text(result))
        self.assertIn('["rejected","rejected"]', text(result))
        result = await b.run('return (await Promise.allSettled([tools.first(1),tools.first([]),tools.first()])).map(x=>x.status);')
        self.assertFalse(result["is_error"], text(result))
        self.assertIn('["rejected","rejected","rejected"]', text(result))
        self.assertEqual(len(b.calls), 2)
        result = await b.run('await tools.unavailable({});')
        self.assertTrue(result["is_error"])
        self.assertEqual(len(b.calls), 2)

    async def test_schema_less_nontext_rejected_and_host_call_cap(self):
        b = await self.broker(lambda m: {"value": {"bad": True}} if m["method"] == "composition/call" else None)
        result = await b.run('return await tools.plain({});')
        self.assertTrue(result["is_error"])
        self.assertIn("non-text", text(result))
        b.context["limits"]["max_calls"] = 2
        result = await b.run('for(let i=0;i<3;i++)await tools.first({});')
        self.assertTrue(result["is_error"])
        self.assertIn("exceeded 2 nested", text(result))
        self.assertEqual(len(b.calls), 3)  # one prior + two admitted

    async def test_aliases_discovery_and_collision_binding(self):
        b = await self.broker()
        b.context["tools"] = [{**TOOLS[0], "name": "mcp__github__list-issues", "description": "Find repository issues"},
                              {**TOOLS[0], "name": "mcp__github__list_issues", "description": "Must not replace first alias"}]
        result = await b.run('text(await searchTools("repository issues",{namespace:"github"}));text(await describeTool("mcp__github__list_issues"));text(await describeNamespace("github"));text(ALL_TOOLS);return await tools.mcp__github__list_issues({});')
        self.assertFalse(result["is_error"], text(result))
        self.assertIn("mcp__github__list_issues", text(result))
        self.assertEqual(b.calls[0]["params"]["name"], "mcp__github__list-issues")

    async def test_images_before_commit_and_failure_prevents_commit(self):
        b = await self.broker()
        result = await b.run(f'image("data:image/png;base64,{PNG}");store("image",1);')
        self.assertFalse(result["is_error"], text(result))
        self.assertEqual(result["content"][1]["artifact_id"], "artifact:test")
        order = [m["method"] for m in b.messages if m.get("method") in ("artifact/publish", "composition/store")]
        self.assertEqual(order, ["artifact/publish", "composition/store"])
        failed = await self.broker(artifacts=False)
        result = await failed.run(f'image("data:image/png;base64,{PNG}");store("bad",1);')
        self.assertTrue(result["is_error"])
        self.assertIn("artifacts feature", text(result))
        self.assertFalse(failed.commits)

    async def test_cancellation_during_artifact_never_commits(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "artifact/publish" else None)
        id_ = await b.start_code(f'image("data:image/png;base64,{PNG}");store("bad",1);')
        artifact = await b.wait(lambda m: m.get("method") == "artifact/publish")
        await b.cancel(id_)
        self.assertEqual((await b.wait(lambda m: m.get("id") == id_))["error"]["code"], -32800)
        await b.wait(lambda m: m.get("method") == "$/cancelRequest" and m["params"]["id"] == artifact["id"])
        self.assertFalse(b.commits)

    async def test_sidecar_value_and_zero_token_spill(self):
        b = await self.broker()
        value = {"large": "🌱" * 300000}
        ref = host_file(self.scratch, value)
        self.assertGreater(ref["bytes"], FRAME_BYTES)
        b.handler = lambda m: {"value_file": ref} if m["method"] == "composition/call" else None
        result = await b.run('const v=await tools.first({});return v.large.length;')
        self.assertFalse(result["is_error"], text(result))
        self.assertTrue(text(result).endswith("\n600000"))
        self.assertFalse((self.scratch / ref["path"]).exists())
        result = await b.run('// @options: {"max_output_tokens":0}\ntext("🌱".repeat(20000));')
        self.assertTrue(result["metadata"]["output_truncated"])
        self.assertEqual(Path(result["metadata"]["full_output_path"]).read_text(), "🌱" * 20000)
        self.assertLessEqual(len(text(result).encode()), OUTPUT_BYTES)
        path = Path(result["metadata"]["full_output_path"])
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)

    async def test_deadline_interrupts_spinning_vm_and_keeps_runner_warm(self):
        b = await self.broker()
        self.assertFalse((await b.run('return "warm up";'))["is_error"])
        runner = await self.assert_runner(b)
        result = await b.run('// @options: {"timeout_ms":1200}\ntext("before timeout");store("bad",1);while(true){}')
        self.assertTrue(result["is_error"])
        self.assertRegex(text(result).lower(), "timeout|timed out|interrupt")
        self.assertIn("before timeout", text(result))
        self.assertEqual(result["metadata"]["error_kind"], "timeout")
        self.assertFalse(b.commits)
        self.assertFalse((await b.run('return 1;'))["is_error"])
        self.assertEqual(await self.assert_runner(b), runner, "An interrupt must not kill the warm runner")

    async def test_waiting_call_deadline_sends_reverse_cancel(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/call" else None)
        b.context["limits"]["timeout_ms"] = 1200
        result = await b.run('store("bad",1);return await tools.first({});')
        self.assertTrue(result["is_error"])
        self.assertEqual(result["metadata"]["error_kind"], "timeout")
        self.assertFalse(b.commits)
        self.assertEqual(len(b.calls), 1)
        await b.wait(lambda m: m.get("method") == "$/cancelRequest" and m["params"]["id"] == b.calls[0]["id"])
        b.handler = None
        self.assertFalse((await b.run('return "fresh runner";'))["is_error"])

    async def test_four_call_limiter_and_unawaited_reverse_cancellation(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/call" else None)
        id_ = await b.start_code('await Promise.all(Array.from({length:20},(_,i)=>tools.first({value:i})));')
        await b.wait(lambda _: len(b.calls) == 4)
        await asyncio.sleep(0.05)
        self.assertEqual(len(b.calls), 4)
        await b.cancel(id_)
        self.assertEqual((await b.wait(lambda m: m.get("id") == id_))["error"]["code"], -32800)
        for call in b.calls:
            await b.wait(lambda m: m.get("method") == "$/cancelRequest" and m["params"]["id"] == call["id"])
        for ending, fails in (('return "done";', False), ('throw Error("expected");', True)):
            prior, commits = len(b.calls), len(b.commits)
            result = await b.run('store("terminal",true);for(let i=0;i<20;i++)tools.first({value:i});' + ending)
            self.assertEqual(result["is_error"], fails, text(result))
            self.assertEqual(len(b.commits) - commits, 0 if fails else 1)
            self.assertLessEqual(len(b.calls) - prior, 4)
            for call in b.calls[prior:]:
                await b.wait(lambda m: m.get("method") == "$/cancelRequest" and m["params"]["id"] == call["id"])

    async def test_host_reverse_cancel_cannot_be_caught_to_commit(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/call" else None)
        for use_error in (False, True):
            id_ = await b.start_code('try{await tools.first({});}catch(e){}store("bad",1);')
            call = await b.wait(lambda m: m.get("method") == "composition/call" and m["params"]["parent_request_id"] == id_)
            if use_error:
                await b.deny(call, -32800, "Request cancelled")
            else:
                await b.cancel(call["id"])
            self.assertEqual((await b.wait(lambda m: m.get("id") == id_))["error"]["code"], -32800)
        self.assertFalse(b.commits)

    async def test_late_sidecar_is_cleaned_without_reviving_parent(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/context" else None)
        id_ = await b.start_code('store("bad",1);')
        request = await b.wait(lambda m: m.get("method") == "composition/context")
        await b.cancel(id_)
        await b.wait(lambda m: m.get("id") == id_)
        ref = host_file(self.scratch, context())
        await b.respond(request, {"context_file": ref})
        menu = await b.request("menu/collect", {"context": {}})
        await b.wait(lambda m: m.get("id") == menu)
        self.assertFalse((self.scratch / ref["path"]).exists())
        self.assertEqual(sum(m.get("id") == id_ for m in b.messages), 1)
        self.assertFalse(b.commits)

    async def test_output_and_memory_limits_prevent_commit(self):
        b = await self.broker()
        for code in ('store("bad",1);text("x".repeat(17*1024*1024));',
                     'store("bad",1);for(let i=0;i<4097;i++)text(i);',
                     f'store("bad",1);for(let i=0;i<65;i++)image("data:image/png;base64,{PNG}");',
                     'store("bad",1);const rows=new Array(40000000).fill(1);return rows.length;'):
            result = await b.run(code)
            self.assertTrue(result["is_error"], text(result))
        self.assertFalse(b.commits)

    async def test_commands_malformed_requests_and_oversized_frame(self):
        b = await self.broker()
        command = await b.request("command/execute", {"name": "codemode", "arguments": ["status"], "context": {}})
        status = (await b.wait(lambda m: m.get("id") == command))["result"]["text"]
        self.assertIn("No Node or Python runtime is required", status)
        for arguments in ({"code": ""}, {"code": "return 1", "extra": True}, {"code": "x" * 65537},
                          {"code": '// @options: {"timeout_ms":0}\nreturn 1'},
                          {"code": '// @options: {"unknown":1}\nreturn 1'}):
            invalid = await b.request("tool/call", {"name": "codemode", "arguments": arguments, "context": {}})
            self.assertEqual((await b.wait(lambda m: m.get("id") == invalid))["error"]["code"], -32602)
        self.assertFalse(any(m.get("method", "").startswith("composition/") for m in b.messages))
        b.process.stdin.write(b" " * (FRAME_BYTES + 2))
        await b.process.stdin.drain()
        self.assertEqual(await asyncio.wait_for(b.process.wait(), 3), 1)

    async def test_repeated_cancel_reaps_the_runner_and_respawns(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/call" else None)
        runners = []
        for _ in range(3):
            id_ = await b.start_code('store("bad",1);await tools.first({});')
            call = await b.wait(lambda m: m.get("method") == "composition/call" and m["params"]["parent_request_id"] == id_)
            child = await self.assert_runner(b)
            runners.append(child)
            for _ in range(3):
                await b.cancel(id_)
                await asyncio.sleep(0)
            self.assertEqual((await b.wait(lambda m: m.get("id") == id_))["error"]["code"], -32800)
            await b.wait(lambda m: m.get("method") == "$/cancelRequest" and m["params"]["id"] == call["id"])
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)
            barrier = await b.request("menu/collect", {"context": {}})
            await b.wait(lambda m: m.get("id") == barrier)
            self.assertEqual(sum(m.get("id") == id_ for m in b.messages), 1)
            self.assertFalse(any(parent == b.process.pid for parent, _ in (await process_table()).values()))
        self.assertEqual(len(set(runners)), 3, "Each cancelled script must get a fresh runner")
        self.assertFalse(b.commits)
        self.assertFalse((await b.run('return "still-ready";'))["is_error"])

    async def test_cancel_after_context_release_never_leaks_runner(self):
        b = await self.broker(lambda m: HOLD if m["method"] == "composition/context" else None)
        for _ in range(5):
            id_ = await b.start_code('store("bad",1);while(true){}')
            request = await b.wait(lambda m: m.get("method") == "composition/context" and m["params"]["parent_request_id"] == id_)
            await b.respond(request, b.context)
            for _ in range(3):
                await b.cancel(id_)
                await asyncio.sleep(0)
            self.assertEqual((await b.wait(lambda m: m.get("id") == id_))["error"]["code"], -32800)
            barrier = await b.request("menu/collect", {"context": {}})
            await b.wait(lambda m: m.get("id") == barrier)
            self.assertEqual(sum(m.get("id") == id_ for m in b.messages), 1)
            self.assertFalse(any(parent == b.process.pid for parent, _ in (await process_table()).values()))
        self.assertFalse(b.commits)
        b.handler = None
        self.assertFalse((await b.run('return 1;'))["is_error"])

    async def test_shutdown_eof_and_signal_reap_runner(self):
        for how in ("shutdown", "eof", "signal"):
            b = await self.broker(lambda m: HOLD if m["method"] == "composition/call" else None)
            id_ = await b.start_code('await tools.first({});')
            await b.wait(lambda m: m.get("method") == "composition/call")
            child = await self.assert_runner(b)
            if how == "shutdown":
                request = await b.request("shutdown", {})
                self.assertEqual((await b.wait(lambda m: m.get("id") == id_))["error"]["code"], -32800)
                self.assertEqual((await b.wait(lambda m: m.get("id") == request))["result"], {})
            elif how == "eof":
                b.process.stdin.close()
            else:
                b.process.send_signal(signal.SIGTERM)
            self.assertEqual(await asyncio.wait_for(b.process.wait(), 5), 0)
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)
            self.assertFalse(b.commits)


class Native(EngineCases, unittest.IsolatedAsyncioTestCase):
    engine = "native"


class Wasi(EngineCases, unittest.IsolatedAsyncioTestCase):
    engine = "wasi"


class BundleEntrypoint(unittest.IsolatedAsyncioTestCase):
    """The shipped entrypoint resolves the Rust executable without Node/Python."""

    async def test_bundle_entrypoint_serves_the_real_wire_with_empty_path(self):
        bundle = Path(__file__).resolve().parents[1]
        scratch = Path(self.temp_dir()).resolve() / "scratch"
        scratch.mkdir(mode=0o700)
        environment = {
            "PATH": "",
            "HOME": os.environ.get("HOME", ""),
            "OCTET_EXTENSION_DIR": str(bundle),
            "OCTET_CODEMODE_BINARY": str(RUNNER),
            "OCTET_EXTENSION_SCRATCH": str(scratch),
        }
        process = await asyncio.create_subprocess_exec(
            str(bundle / "bin/codemode"), "serve", "--engine", "wasi",
            cwd=bundle, env=environment, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, limit=FRAME_BYTES + 1)
        try:
            process.stdin.write((dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": offer()}) + "\n").encode())
            await process.stdin.drain()
            response = loads((await process.stdout.readline()).decode())
            self.assertEqual(response["result"]["tools"][0]["name"], "codemode")
            process.stdin.write((dumps({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}) + "\n").encode())
            await process.stdin.drain()
            self.assertEqual(loads((await process.stdout.readline()).decode())["id"], 2)
            self.assertEqual(await asyncio.wait_for(process.wait(), 10), 0)
        finally:
            if process.returncode is None:
                process.kill()
                await process.wait()
            process.stdin.close()
            await process.stderr.read()

    def temp_dir(self):
        if not hasattr(self, "_temp"):
            self._temp = tempfile.TemporaryDirectory(prefix="codemode-bundle-")
            self.addCleanup(self._temp.cleanup)
        return self._temp.name


if __name__ == "__main__":
    unittest.main()
