"""Real native child processes; the companion host-check uses ExtensionProcess."""
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
BIN = Path(os.environ.get("OCTET_NATIVE_BIN_DIR", ROOT / "examples/extensions/native-hello/build"))
MAX_FRAME = 1024 * 1024
MAX_TEXT = 128 * 1024


def offer(tool="hello"):
    return {
        "api_version": "0.4", "octet_version": "0.8.2", "extension": {"name": "test"},
        "workspace": str(ROOT), "capabilities": {"filesystem": "none", "process": False, "network": False},
        "host": {}, "contributes": {"tools": [tool]}, "flag_values": [],
        "protocol": {"version": "0.4", "required_features": ["request_cancellation", "content_parts"],
                     "optional_features": ["request_progress", "artifacts", "dynamic_tools", "unknown_future_optional"],
                     "limits": {"max_concurrent_requests": 64}},
    }


class Peer:
    def __init__(self, command):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.frames = queue.Queue()
        self.diagnostics = bytearray()
        def read():
            try:
                for frame in self.process.stdout:
                    self.frames.put(frame)
            finally:
                self.frames.put(None)
        def diagnostics():
            self.diagnostics.extend(self.process.stderr.read())
        self.reader = threading.Thread(target=read, daemon=True)
        self.stderr_reader = threading.Thread(target=diagnostics, daemon=True)
        self.reader.start()
        self.stderr_reader.start()

    def raw(self, data):
        self.process.stdin.write(data)
        self.process.stdin.flush()

    def send(self, method, params, request_id=None):
        value = {"jsonrpc": "2.0", "method": method, "params": params}
        if request_id is not None:
            value["id"] = request_id
        self.raw(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode() + b"\n")

    def receive(self, timeout=3):
        frame = self.frames.get(timeout=timeout)
        if frame is None:
            raise AssertionError(f"native process closed: status={self.process.poll()}, stderr={bytes(self.diagnostics)!r}")
        assert frame.endswith(b"\n") and len(frame) - 1 <= MAX_FRAME, len(frame)
        value = json.loads(frame)
        assert value["jsonrpc"] == "2.0"
        assert ("result" in value) != ("error" in value), value
        return value

    def initialize(self, tool="hello"):
        self.send("initialize", offer(tool), 1)
        reply = self.receive()
        assert reply["id"] == 1, reply
        result = reply["result"]
        assert result["api_version"] == "0.4" and result["commands"] == [], result
        assert result["protocol"] == {"version": "0.4", "features": ["request_cancellation", "content_parts"], "limits": {"max_concurrent_requests": 1}}, result
        assert [t["name"] for t in result["tools"]] == [tool], result
        assert result["tools"][0]["parameters"]["type"] == "object"
        return result

    def call(self, args, request_id=2, tool="hello", context=None):
        self.send("tool/call", {"name": tool, "arguments": args, "context": {} if context is None else context}, request_id)
        reply = self.receive()
        assert reply["id"] == request_id, reply
        return reply

    def shutdown(self):
        self.send("shutdown", {}, 999)
        reply = self.receive()
        assert reply == {"jsonrpc": "2.0", "id": 999, "result": {}}, reply
        assert self.process.wait(timeout=2) == 0, bytes(self.diagnostics)
        self.reader.join(timeout=1)
        assert self.frames.get(timeout=1) is None, "extra terminal frame"

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=2)
        self.reader.join(timeout=1)
        self.stderr_reader.join(timeout=1)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()

    def __enter__(self): return self
    def __exit__(self, *unused): self.close()


class NativeProcesses(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        required = [f"{tool}-{lang}" for tool in ("hello", "probe") for lang in ("rust", "c", "cpp")]
        missing = [name for name in required if not (BIN / name).is_file()]
        if missing:
            raise RuntimeError(f"Build native examples first: {missing}")

    def hellos(self):
        return [[str(BIN / f"hello-{lang}")] for lang in ("rust", "c", "cpp")]

    def probes(self):
        return [[str(BIN / "probe-rust")], [str(BIN / "probe-c"), "process"], [str(BIN / "probe-cpp")]]

    def test_all_languages_real_tools_and_schemas(self):
        for command in self.hellos():
            with self.subTest(command=command), Peer(command) as peer:
                result = peer.initialize()
                schema = result["tools"][0]["parameters"]
                self.assertEqual(schema["properties"]["name"]["type"], "string")
                self.assertIn("name", schema["required"])
                self.assertFalse(schema["additionalProperties"])
                for text in ("world", "λ😀", "a\0b", "😀" * 256):
                    reply = peer.call({"name": text})["result"]
                    self.assertEqual(reply["content"], [{"type": "text", "text": f"Hello, {text}!"}])
                    self.assertFalse(reply["is_error"])
                peer.shutdown()

    def test_input_validation_before_domain_code(self):
        for command in self.hellos():
            with self.subTest(command=command), Peer(command) as peer:
                peer.initialize()
                for args in ({}, {"name": 1}, {"name": None}, {"name": "x", "extra": 1},
                             {"name": "x" * 257}, {"name": "x", "delay_ms": -1},
                             {"name": "x", "delay_ms": 5001}, {"name": "x", "delay_ms": True}):
                    self.assertEqual(peer.call(args)["error"]["code"], -32602)
                for params in ({"name": "hello", "arguments": {}, "context": []},
                               {"name": "hello", "arguments": {}, "context": {}, "catalog_revision": 0},
                               {"name": "hello", "arguments": []},
                               {"arguments": {}, "context": {}}):
                    peer.send("tool/call", params, 3)
                    self.assertEqual(peer.receive()["error"]["code"], -32602)
                self.assertEqual(peer.call({"name":"x"},tool="unknown")["error"]["code"], -32601)
                peer.shutdown()

    def test_cancel_and_shutdown_while_work_is_active(self):
        for command in self.hellos():
            with self.subTest(command=command), Peer(command) as peer:
                peer.initialize()
                peer.send("tool/call", {"name": "hello", "arguments": {"name": "cancel", "delay_ms": 5000}, "context": {}}, 42)
                peer.send("$/cancelRequest", {"id": 42, "reason": "user"})
                reply = peer.receive()
                self.assertEqual((reply["id"],reply["error"]["code"]),(42,-32800))
                peer.send("$/cancelRequest", {"id": 42})  # late and duplicate are harmless
                self.assertEqual(peer.call({"name":"next"})["result"]["content"][0]["text"],"Hello, next!")
                peer.send("tool/call", {"name": "hello", "arguments": {"name": "shutdown", "delay_ms": 5000}, "context": {}}, 43)
                peer.send("shutdown", {}, 999)
                replies = [peer.receive(), peer.receive()]
                self.assertEqual({r["id"] for r in replies}, {43,999})
                self.assertEqual(next(r for r in replies if r["id"] == 43)["error"]["code"],-32800)
                self.assertEqual(next(r for r in replies if r["id"] == 999)["result"],{})
                self.assertEqual(peer.process.wait(timeout=2),0)

    def test_optional_services_fail_explicitly(self):
        for command in self.hellos():
            with self.subTest(command=command), Peer(command) as peer:
                peer.initialize()
                for method in ("hook/run", "command/execute", "status/collect", "menu/collect", "tool/render", "context/collect"):
                    peer.send(method, {}, 10)
                    self.assertEqual(peer.receive()["error"]["code"], -32601)
                peer.send("shutdown", {"unknown": True}, 11)
                self.assertEqual(peer.receive()["error"]["code"], -32602)
                peer.shutdown()

    def test_negotiation_rejects_wrong_wire_features_and_declarations(self):
        cases = []
        for version in ("0.1", "0.2", "0.3"):
            value = offer(); value["api_version"] = version; cases.append(value)
        value = offer(); value["protocol"]["version"] = "0.2"; cases.append(value)
        value = offer(); value["protocol"]["required_features"].append("artifacts"); cases.append(value)
        value = offer(); value["protocol"]["required_features"] = ["content_parts"]; cases.append(value)
        value = offer(); value["protocol"]["required_features"].append("content_parts"); cases.append(value)
        value = offer(); value["protocol"]["optional_features"].append("content_parts"); cases.append(value)
        for limit in (0, True, -1, "1"):
            value = offer(); value["protocol"]["limits"]["max_concurrent_requests"] = limit; cases.append(value)
        for extra in ({"hooks":["before_prompt"]},{"ui":["status"]},{"commands":["hello"]},{"presentation":True},{"flags":[{"name":"flag"}]}):
            value = offer(); value["contributes"].update(extra); cases.append(value)
        value = offer(); value["contributes"]["tools"] = []; cases.append(value)
        value = offer(); del value["contributes"]["tools"]; cases.append(value)
        value = offer(); value["contract"] = {}; cases.append(value)
        for command in self.hellos():
            for value in cases:
                with self.subTest(command=command,offer=value), Peer(command) as peer:
                    peer.send("initialize",value,1)
                    self.assertIn("error",peer.receive())
                    self.assertNotEqual(peer.process.wait(timeout=2),0)

    def test_hostile_frames_and_envelopes_all_languages(self):
        frames = (b"{\n", b"[]\n", b' {"jsonrpc":"1.0","method":"shutdown","params":{},"id":2}\n',
                  b'{"jsonrpc":"2.0","method":"shutdown","params":{},"id":null}\n',
                  b'{"jsonrpc":"2.0","method":"shutdown","params":{},"id":true}\n',
                  b'{"jsonrpc":"2.0","method":"shutdown","params":{},"id":2,"result":{}}\n',
                  b'{"jsonrpc":"2.0","method":"shutdown","params":{},"params":{},"id":2}\n')
        for command in self.hellos():
            for frame in frames:
                with self.subTest(command=command,frame=frame), Peer(command) as peer:
                    peer.initialize()
                    peer.raw(frame)
                    self.assertIn(peer.receive()["error"]["code"],(-32700,-32600))
                    peer.shutdown()
            with Peer(command) as peer:
                peer.initialize()
                peer.raw(b'{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}\r\n')
                self.assertEqual(peer.receive()["result"],{})
                self.assertEqual(peer.process.wait(timeout=2),0)

    def test_frame_limit_exact_oversized_utf8_and_eof(self):
        for command in self.hellos():
            with self.subTest(command=command), Peer(command) as peer:
                peer.initialize()
                request = b'{"jsonrpc":"2.0","id":9,"method":"unknown","params":{}}'
                peer.raw(request + b' ' * (MAX_FRAME-len(request)) + b'\n')
                self.assertEqual(peer.receive()["error"]["code"],-32601)
                peer.shutdown()
            with Peer(command) as peer:
                peer.initialize()
                peer.raw(b' ' * (MAX_FRAME+1) + b'\n')
                self.assertEqual(peer.receive()["error"]["code"],-32700)
                self.assertNotEqual(peer.process.wait(timeout=2),0)
            with Peer(command) as peer:
                peer.initialize()
                peer.raw(b'\xff\n')
                self.assertEqual(peer.receive()["error"]["code"],-32700)
                peer.shutdown()
            with Peer(command) as peer:
                peer.initialize()
                peer.raw(b'{"jsonrpc":')
                peer.process.stdin.close()
                self.assertEqual(peer.receive()["error"]["code"],-32700)
                self.assertNotEqual(peer.process.wait(timeout=2),0)
            with Peer(command) as peer:
                peer.initialize()
                peer.send("tool/call", {"name":"hello","arguments":{"name":"EOF","delay_ms":5000},"context":{}}, 42)
                peer.process.stdin.close()
                self.assertEqual(peer.receive()["error"]["code"],-32800)
                self.assertEqual(peer.process.wait(timeout=2),0)

    def test_bounded_init_and_uncooperative_shutdown(self):
        with Peer([str(BIN / 'hello-rust')]) as peer:
            start = time.monotonic()
            self.assertNotEqual(peer.process.wait(timeout=7),0)
            self.assertLess(time.monotonic()-start,6.5)
        with Peer([str(BIN / 'probe-rust')]) as peer:
            peer.initialize('probe')
            peer.send('tool/call',{'name':'probe','arguments':{'mode':'uncooperative'},'context':{}},42)
            time.sleep(.1)
            peer.send('shutdown',{},999)
            self.assertEqual(peer.process.wait(timeout=2),70)

    def test_single_admission_and_duplicate_id(self):
        for command in self.hellos():
            with self.subTest(command=command), Peer(command) as peer:
                peer.initialize()
                params = {'name':'hello','arguments':{'name':'pending','delay_ms':5000},'context':{}}
                peer.send('tool/call',params,42)
                peer.send('tool/call',params,43)
                reply = peer.receive()
                self.assertEqual((reply['id'],reply['error']['code']),(43,-32000))
                peer.send('$/cancelRequest',{'id':42})
                self.assertEqual(peer.receive()['error']['code'],-32800)
                peer.shutdown()
            with Peer(command) as peer:
                peer.initialize()
                peer.send('tool/call',params,42)
                peer.send('tool/call',params,42)
                replies = [peer.receive(),peer.receive()]
                self.assertEqual({r['error']['code'] for r in replies},{-32600,-32800})
                self.assertNotEqual(peer.process.wait(timeout=2),0)

    def test_domain_errors_panics_cpp_exceptions_and_c_lifetimes(self):
        result = subprocess.run([str(BIN/'probe-c')],capture_output=True,timeout=3)
        self.assertEqual(result.returncode,0,result.stderr)
        for command in self.probes():
            with self.subTest(command=command), Peer(command) as peer:
                peer.initialize('probe')
                for text in ('lifetime λ😀\0 bytes', '', 'x'*MAX_TEXT):
                    reply = peer.call({'mode':'lifetime','text':text},tool='probe')['result']
                    self.assertEqual(reply['content'][0]['text'],text)
                    self.assertFalse(reply['is_error'])
                reply = peer.call({'mode':'error'},tool='probe')['result']
                self.assertTrue(reply['is_error'])
                self.assertEqual(reply['content'][0]['text'],'domain failure')
                if 'probe-rust' in command[0]:
                    for mode in ('panic','oversized'):
                        self.assertEqual(peer.call({'mode':mode},tool='probe')['error']['code'],-32603)
                    self.assertEqual(peer.call({'mode':'max-result'},tool='probe')['result']['content'][0]['text'],'\0'*MAX_TEXT)
                    context = {'resource_owner':{'session_id':'host-owner','extension_instance_id':'host-instance','process_generation':7}}
                    result = peer.call({'mode':'context'},tool='probe',context=context)['result']['content'][0]['text']
                    self.assertEqual(json.loads(result),context)
                elif 'probe-cpp' in command[0]:
                    for mode in ('throw','status'):
                        self.assertTrue(peer.call({'mode':mode},tool='probe')['result']['is_error'])
                else:
                    self.assertEqual(peer.call({'mode':'omit'},tool='probe')['error']['code'],-32603)
                    self.assertTrue(peer.call({'mode':'duplicate'},tool='probe')['result']['is_error'])
                    self.assertEqual(peer.call({'mode':'typed','text':'typed','number':-9007199254740991,'flag':True},tool='probe')['result']['content'][0]['text'],'typed')
                peer.shutdown()

    def test_flood_depth_and_invalid_cancellation_are_bounded(self):
        with Peer([str(BIN/'hello-rust')]) as peer:
            peer.initialize()
            deep = {}; cursor = deep
            for _ in range(40):
                cursor['next'] = {}; cursor = cursor['next']
            peer.send('unknown',deep,10)
            self.assertEqual(peer.receive()['error']['code'],-32602)
            peer.send('unknown',{'nodes':[0]*17000},11)
            self.assertEqual(peer.receive()['error']['code'],-32602)
            for _ in range(6):  # depth/node refusals already consumed two of eight
                peer.raw(b'[]\n')
                self.assertEqual(peer.receive()['error']['code'],-32600)
            self.assertNotEqual(peer.process.wait(timeout=2),0)
        with Peer([str(BIN/'hello-rust')]) as peer:
            peer.initialize()
            peer.send('$/cancelRequest',{'id':True})
            peer.send('$/cancelRequest',{'id':42},12) # requests are not notifications
            self.assertEqual(peer.receive()['error']['code'],-32602)
            peer.shutdown()


if __name__ == '__main__': unittest.main(verbosity=2)
