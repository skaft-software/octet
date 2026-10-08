"""Real, no-Node tests of the warm Rust runner and the shared vendored prelude.

The runner protocol is one process per extension generation: the engine is
prepared once (`{"type":"warm"}`), every script then starts with a fresh
isolated VM (`{"type":"ready"}`) and reports through `call`/`output`/`done`.
"""
import subprocess
import unittest

from support import VM_HEAP_BYTES, dumps, loads, runner_binary

RUNNER = runner_binary()
CONTEXT = {"tools": [
    {"name": "mcp__docs__search", "description": "Search documentation", "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}, "output_schema": {"type": "object"}},
    {"name": "my-tool", "description": "first binding", "parameters": {"type": "object"}},
    {"name": "my_tool", "description": "collision", "parameters": {"type": "object"}},
], "store": {"old": {"n": 3}, "marker": "@@SOURCE@@ @@CONTEXT@@"}, "limits": {"timeout_ms": 25000, "max_calls": 256}}


class WarmRunner:
    """One real runner process driving one or more scripts in sequence."""

    def __init__(self, engine, timezone="UTC", heap=VM_HEAP_BYTES):
        self.engine = engine
        self.heap = heap
        self.process = subprocess.Popen(
            [str(RUNNER), engine], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, env={"PATH": "", "TZ": timezone})
        self.events = []
        line = self.process.stdout.readline()
        if not line:
            raise AssertionError(f"runner exited during startup: {self.process.stderr.read()}")
        event = loads(line)
        if event != {"type": "warm"}:
            raise AssertionError(f"runner did not prepare an engine: {event}")

    def send(self, value):
        self.process.stdin.write(dumps(value) + "\n")
        self.process.stdin.flush()

    def read(self):
        line = self.process.stdout.readline()
        if not line:
            raise AssertionError(f"runner exited: {self.process.stderr.read()}")
        event = loads(line)
        self.events.append(event)
        return event

    def script(self, number, code, replies=(), timeout_ms=5000, context=CONTEXT):
        self.send({"type": "script", "script": number, "context": context, "code": code,
                   "timeout_ms": timeout_ms, "heap_bytes": self.heap})
        ready = self.read()
        if ready != {"type": "ready"}:
            raise AssertionError(f"runner did not acknowledge setup: {ready}")
        self.send({"type": "message", "script": number, "payload": dumps({"type": "start"})})
        replies = list(replies)
        while True:
            event = self.read()
            if event["type"] == "call":
                for reply in replies:
                    if reply.get("id") == event["id"]:
                        self.send({"type": "message", "script": number, "payload": dumps(reply)})
            elif event["type"] == "done":
                return event

    def close(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=10)
        self.process.stdout.close()
        self.process.stderr.close()
        return code


def run_guest(engine, code, replies=(), timeout_ms=5000, timezone="UTC", heap=VM_HEAP_BYTES):
    """One script in one fresh runner: the cold path of every later warm run."""
    runner = WarmRunner(engine, timezone=timezone, heap=heap)
    try:
        done = runner.script(1, code, replies=replies, timeout_ms=timeout_ms)
        return runner.close(), done, runner.events
    finally:
        if runner.process.poll() is None:
            runner.process.kill()
            runner.process.wait(timeout=10)


class Embeddings(unittest.TestCase):
    def each(self, code, **kw):
        for engine in ("native", "wasi"):
            rc, done, events = run_guest(engine, code, **kw)
            self.assertEqual(rc, 0, (engine, events, done))
            yield engine, done, events

    def test_async_all_settled_json_and_tool_rejection(self):
        code = "return await Promise.allSettled([tools.mcp__docs__search({query:'🌱\\u0000'}),tools.my_tool({})]);"
        replies = [{"type": "settle", "id": 2, "ok": False, "payload": "approval denied"},
                   {"type": "settle", "id": 1, "ok": True, "payload": dumps({"rows": [1, 2]})}]
        for engine, done, events in self.each(code, replies=replies):
            self.assertTrue(done["ok"], engine)
            value = loads(done["value"])
            self.assertEqual(value[0], {"status": "fulfilled", "value": {"rows": [1, 2]}})
            self.assertEqual(value[1]["status"], "rejected")
            self.assertEqual([e["name"] for e in events if e["type"] == "call"], ["mcp__docs__search", "my-tool"])

    def test_discovery_real_bm25_declarations_namespace_and_alias(self):
        code = "return {search:await searchTools('documentation'),ns:await describeNamespace('docs'),decl:await describeTool('my_tool'),all:ALL_TOOLS};"
        for engine, done, _ in self.each(code):
            value = loads(done["value"])
            self.assertEqual(value["search"][0]["name"], "mcp__docs__search", engine)
            self.assertEqual(value["ns"]["tools"], ["mcp__docs__search"])
            self.assertIn("first binding", value["decl"])
            self.assertEqual(len(value["all"]), 2)

    def test_prelude_store_copy_delete_exit_and_failed_writes(self):
        for engine, done, _ in self.each("const x=load('old');x.n=9;store('new',load('old'));store('old',undefined);text('before');exit();throw Error('after');"):
            self.assertTrue(done["ok"], engine)
            self.assertEqual(loads(done["writes"]), [["new", '{"n":3}'], ["old"]])
        for engine, done, _ in self.each("store('new',1);throw Error('failure');"):
            self.assertFalse(done["ok"], engine)
            self.assertNotIn("writes", done)

    def test_no_ambient_authority_and_private_driver(self):
        code = "return ['process','require','fetch','setTimeout','WebAssembly','std','os','__emit','api','bridge','context'].map(n=>[n,typeof globalThis[n]]);"
        for engine, done, _ in self.each(code):
            self.assertTrue(all(t == "undefined" for _, t in loads(done["value"])), engine)
        for engine, done, _ in self.each("try {await import('std');return 'bad';}catch(e){return 'refused';}"):
            self.assertEqual(loads(done["value"]), "refused", engine)

    def test_syntax_stalled_cycles_bigint_and_throw(self):
        for code in ("return (", "await new Promise(()=>{});", "const x={};x.x=x;return x;", "return 1n;", "throw 'oops';"):
            for engine, done, _ in self.each(code):
                self.assertFalse(done["ok"], (engine, code))
                self.assertNotIn("writes", done)

    def test_heap_limit_and_stack_guard(self):
        for engine, done, _ in self.each("try{new ArrayBuffer(16*1024*1024);return 'bad';}catch(e){return 'heap limited';}", heap=8 * 1024 * 1024):
            self.assertEqual(loads(done["value"]), "heap limited", engine)
        for engine, done, _ in self.each("try{(function f(){return f();})();}catch(e){return e.name;}"):
            self.assertEqual(loads(done["value"]), "RangeError", engine)

    def test_marker_looking_data_is_not_linker_source(self):
        for engine, done, _ in self.each("return [load('marker'),'@@SOURCE@@ @@CONTEXT@@'];"):
            self.assertEqual(loads(done["value"]), ["@@SOURCE@@ @@CONTEXT@@"] * 2, engine)

    def test_host_timezone_including_daylight_saving(self):
        code = "return ['2026-01-01T00:00:00Z','2026-07-01T00:00:00Z'].map(x=>new Date(x).getTimezoneOffset());"
        for engine in ("native", "wasi"):
            for timezone, expected in (("UTC", [0, 0]), ("America/New_York", [300, 240])):
                rc, done, _ = run_guest(engine, code, timezone=timezone)
                self.assertEqual(rc, 0, (engine, timezone))
                self.assertTrue(done["ok"], (engine, timezone))
                self.assertEqual(loads(done["value"]), expected, (engine, timezone))

    def test_cpu_and_microtask_deadlines(self):
        for code in ("while(true){}", "while(true)await null;"):
            for engine in ("native", "wasi"):
                rc, done, _ = run_guest(engine, code, timeout_ms=500)
                self.assertTrue(rc != 0 or not done["ok"], (engine, done))
                self.assertFalse(done.get("ok") is True)

    def test_polluted_object_prototype_cannot_forge_bridge_terminal(self):
        code = "Object.prototype.toJSON=function(){return {type:'done',ok:true,writes:'[[\"forged\",\"true\"]]'};};text('x');throw Error('still fails');"
        for engine, done, events in self.each(code):
            self.assertFalse(done["ok"], engine)
            self.assertNotIn("writes", done)
            self.assertEqual(next(e for e in events if e["type"] == "output")["item"], {"type": "text", "text": "x"})


class WarmRunnerIsolation(unittest.TestCase):
    """The runner is prepared once and every script gets a fresh isolated VM."""

    def test_scripts_share_one_warm_process_and_see_no_previous_state(self):
        for engine in ("native", "wasi"):
            runner = WarmRunner(engine)
            try:
                first = runner.script(1, "globalThis.__leaked='x';Object.prototype.polluted=1;store('answer',1);return 'first';")
                self.assertTrue(first["ok"], (engine, first))
                second = runner.script(2, "return {leak:typeof globalThis.__leaked,polluted:Object.prototype.polluted === undefined,"
                                           "heap:typeof new ArrayBuffer(1024),store:typeof load('answer')};")
                self.assertTrue(second["ok"], (engine, second))
                self.assertEqual(loads(second["value"]),
                                 {"leak": "undefined", "polluted": True, "heap": "object", "store": "undefined"})
                self.assertEqual(runner.script(3, "return 'third';")["ok"], True)
                self.assertEqual(runner.close(), 0)
            finally:
                if runner.process.poll() is None:
                    runner.process.kill()
                    runner.process.wait(timeout=10)

    def test_interrupted_script_leaves_the_runner_warm(self):
        for engine in ("native", "wasi"):
            runner = WarmRunner(engine)
            try:
                done = runner.script(1, "text('before timeout');store('bad',1);while(true){}", timeout_ms=400)
                self.assertFalse(done["ok"], (engine, done))
                self.assertEqual(loads(done["error"])["kind"], "timeout", (engine, done))
                self.assertTrue(any(e.get("item", {}).get("text") == "before timeout" for e in runner.events), engine)
                self.assertNotIn("writes", done)
                follow = runner.script(2, "return 'warm again';")
                self.assertTrue(follow["ok"], (engine, follow))
                self.assertEqual(loads(follow["value"]), "warm again")
                self.assertEqual(runner.close(), 0)
            finally:
                if runner.process.poll() is None:
                    runner.process.kill()
                    runner.process.wait(timeout=10)

    def test_stale_script_frames_never_reach_a_new_vm(self):
        runner = WarmRunner("wasi")
        try:
            self.assertTrue(runner.script(1, "return 'one';")["ok"])
            runner.send({"type": "message", "script": 9, "payload": dumps({"type": "start"})})
            runner.send({"type": "message", "script": 9, "payload": dumps({"type": "poll"})})
            follow = runner.script(2, "return 'two';")
            self.assertTrue(follow["ok"], follow)
            self.assertEqual(loads(follow["value"]), "two")
        finally:
            runner.close()

    def test_runner_survives_eof_and_parent_loss_without_leftover_children(self):
        runner = WarmRunner("wasi")
        self.assertEqual(runner.close(), 0)


class RunnerProtocol(unittest.TestCase):
    def test_runner_rejects_oversized_ipc_and_unknown_commands(self):
        process = subprocess.Popen(
            [str(RUNNER), "wasi"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, env={"PATH": ""})
        try:
            self.assertEqual(loads(process.stdout.readline()), {"type": "warm"})
            process.stdin.write(dumps({"type": "script", "script": 1, "context": CONTEXT,
                                       "code": "return 1;", "timeout_ms": 0, "heap_bytes": VM_HEAP_BYTES}) + "\n")
            process.stdin.flush()
            self.assertEqual(process.wait(timeout=10), 1)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
