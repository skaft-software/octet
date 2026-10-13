"""Actual process smoke for independently built single-source templates."""
from pathlib import Path
from test_process import Peer, ROOT

commands = [
    ROOT / "examples/extensions/native-hello/rust/target/debug/hello-rust",
    ROOT / "examples/extensions/native-hello/build/hello-c-template",
    ROOT / "examples/extensions/native-hello/build/hello-cpp-template",
]
for executable in commands:
    if not Path(executable).is_file():
        raise SystemExit(f"Build the standalone Rust and C/C++ Make templates first: {executable}")
    with Peer([str(executable)]) as peer:
        peer.initialize()
        result = peer.call({"name": "template λ😀"})["result"]
        assert result["content"] == [{"type": "text", "text": "Hello, template λ😀!"}]
        assert result["is_error"] is False
        peer.send("tool/call", {"name":"hello", "arguments":{"name":"cancel","delay_ms":5000}, "context":{}}, 42)
        peer.send("$/cancelRequest", {"id":42})
        assert peer.receive()["error"]["code"] == -32800
        peer.shutdown()
    print(f"template process PASS: {executable}")
