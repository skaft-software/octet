"""Ordinary API 0.3 SDK participant: serial reader + bounded tool worker.

The SDK owns exactly one separate cancellable rebinding worker. Neither reader
callback waits for an RPC response; host requests always capture their binding.
"""
import json
import queue
import sys
import threading
import time

SDK_PATH = "__SDK_PATH__"
EXTENSION_NAME = "__EXTENSION_NAME__"
LIFECYCLE_HOOKS = False
sys.path.insert(0, SDK_PATH)
from octet_extension.event_bus import BusError, FieldSpec, HostEventBus, TopicRegistry, TopicSpec
from octet_extension.protocol import RpcError

output_lock = threading.Lock()
state_lock = threading.Condition()
pending = {}
events = []
wire_order = []
next_id = 0
stopping = threading.Event()
commands = queue.Queue(maxsize=16)
raw_binding = ""
captured = None
sdk_mode = False
hold_subscribe_events = 0
registry = TopicRegistry()
# Explicit immutable subscriber schema; no publisher generation is inferred.
if EXTENSION_NAME != "alpha":
    registry.declare(TopicSpec("alpha", "status", (FieldSpec.string("summary", max_bytes=128),)))


def send(value):
    with output_lock:
        print(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")), flush=True)


def result(call_id, value):
    send({"jsonrpc": "2.0", "id": call_id, "result": {
        "content": [], "is_error": False, "metadata": None, "structured_content": value}})


def observe_wire(kind):
    # Reader-only, content-free ordering evidence, including rejected events.
    if len(wire_order) >= 128:
        raise RuntimeError("fixture wire trace bound")
    wire_order.append(kind)


def request_host(method, params, cancelled, accept_result=None):
    global next_id
    deadline = time.monotonic() + 3
    with state_lock:
        if len(pending) >= 8:
            raise BusError(-32012, "fixture_pending_full")
        next_id += 1
        peer_id = "peer-" + str(next_id)
        slot = {"method": method, "accept_result": accept_result, "cancelled": cancelled,
                "deadline": deadline, "done": False, "result": None, "error": None}
        pending[peer_id] = slot
    try:
        send({"jsonrpc": "2.0", "id": peer_id, "method": method, "params": params})
        with state_lock:
            while not slot["done"] and not cancelled.is_set() and not stopping.is_set() and time.monotonic() < deadline:
                state_lock.wait(0.01)
            if not slot["done"]:
                raise BusError(-32011, "fixture_request_cancelled")
            if slot["error"] is not None:
                raise slot["error"]
            # Deterministic SDK regression: keep the requesting tool worker
            # parked after its ACK while the serial reader consumes an event.
            while method == "bus/subscribe" and len(events) < hold_subscribe_events:
                if cancelled.is_set() or stopping.is_set() or time.monotonic() >= deadline:
                    raise BusError(-32011, "fixture_event_wait_cancelled")
                state_lock.wait(0.01)
            return slot["result"]
    finally:
        with state_lock:
            pending.pop(peer_id, None)


bus = HostEventBus(request_host, registry, extension_id=EXTENSION_NAME)


def tools():
    global captured, sdk_mode, hold_subscribe_events
    while not stopping.is_set():
        request = commands.get()
        if request is None:
            return
        if request["method"] == "hook/run":
            try:
                if request["params"]["hook"] == "session_start":
                    bus.declare(name="status", fields=(FieldSpec.string("summary", max_bytes=128),))
                send({"jsonrpc": "2.0", "id": request["id"], "result": {"disposition": {"kind": "continue"}}})
            except (RpcError, BusError) as error:
                send({"jsonrpc": "2.0", "id": request["id"], "error": {"code": error.code, "message": "invalid params"}})
            continue
        args = request["params"]["arguments"]
        method = args["method"]
        params = args.get("params", {})
        try:
            if method == "events":
                with state_lock:
                    state_lock.wait_for(lambda: len(events) >= args.get("count", 0) or stopping.is_set(), timeout=3)
                    value = list(events)
            elif method == "sdk/state":
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    value = bus.snapshot()
                    if value["binding_revision"] >= args.get("revision", 1) and (
                        not args.get("active") or args["active"] in value["subscribed"]):
                        break
                    stopping.wait(0.005)
            elif method == "sdk/declare":
                bus.declare(name="status", fields=(FieldSpec.string("summary", max_bytes=128),))
                value = {"result": bus.snapshot()}
            elif method == "sdk/subscribe":
                sdk_mode = True
                hold_subscribe_events = params.get("wait_for_events", 0)
                try:
                    bus.subscribe("bus.alpha.status")
                    value = {"result": bus.snapshot()}
                finally:
                    hold_subscribe_events = 0
            elif method == "sdk/unsubscribe":
                bus.unsubscribe("bus.alpha.status")
                value = {"result": bus.snapshot()}
            elif method == "sdk/publish":
                value = {"result": bus.publish("bus.alpha.status", params["payload"]).public()}
            elif method == "capture":
                captured = (params["method"], {**params["params"], "binding_id": raw_binding})
                value = {"binding_id": raw_binding}
            elif method == "release":
                saved_method, saved_params = captured
                captured = None
                value = {"result": request_host(saved_method, saved_params, stopping)}
            else:
                # Raw negative fixtures still capture, never rewrite, explicit
                # caller bindings. SDK recovery tests exclusively use sdk/*.
                params = {"binding_id": raw_binding, **params}
                value = {"result": request_host(method, params, stopping)}
        except (RpcError, BusError) as error:
            from octet_extension.api_v03 import ERROR_SPECS
            message = next((message for code, message in ERROR_SPECS.values() if code == error.code), "invalid params")
            value = {"jsonrpc": "2.0", "id": "fixture-error", "error": {"code": error.code, "message": message}}
        result(request["id"], value)


tool_worker = threading.Thread(target=tools, daemon=True)
tool_worker.start()
try:
    for line in sys.stdin:
        request = json.loads(line)
        method = request.get("method")
        if method == "initialize":
            offer = request["params"]["contract"]
            capabilities = offer["required_capabilities"][:]
            methods = offer["required_methods"][:]
            if LIFECYCLE_HOOKS and "hook/run" in offer["optional_methods"]:
                capabilities.append("lifecycle_events")
                methods.append("hook/run")
            if "event_bus" in offer["optional_capabilities"]:
                capabilities.append("event_bus")
                methods.extend(name for name in offer["optional_methods"] if name.startswith("bus/"))
            send({"jsonrpc": "2.0", "id": request["id"], "result": {
                "api_version": "0.3", "contract": {
                    "schema": offer["schema"], "encoding": offer["encoding"],
                    "capabilities": sorted(capabilities), "methods": sorted(methods), "limits": offer["limits"]},
                "tools": [{"name": "probe", "description": "Bus fixture", "parameters": {"type": "object"}}]}})
        elif method == "hook/run":
            commands.put_nowait(request)
        elif method == "tool/call":
            if request["params"]["arguments"]["method"] == "sdk/reader-state":
                with state_lock:
                    held = any(slot["method"] == "bus/subscribe" and slot["done"]
                               and slot["error"] is None for slot in pending.values())
                result(request["id"], {"state": bus.snapshot(), "subscribe_held": held,
                                       "wire_order": list(wire_order)})
            else:
                commands.put_nowait(request)
        elif method == "bus/lifecycle":
            if request["params"]["kind"] == "binding":
                raw_binding = request["params"]["binding_id"]
            bus.accept_lifecycle(request["params"])
        elif method == "bus/event":
            if sdk_mode:
                observe_wire("event")
                try:
                    event = bus.accept_event(request["params"]).public()
                except BusError:
                    continue
            else:
                # Raw transport cases below deliberately bypass only the SDK
                # delivery ledger; sdk/* cases always have a real active ACK.
                event = request["params"]
            with state_lock:
                if len(events) >= 64:
                    raise RuntimeError("fixture event bound")
                events.append(event)
                state_lock.notify_all()
        elif method == "shutdown":
            send({"jsonrpc": "2.0", "id": request["id"], "result": {"terminal": "shutdown"}})
            break
        else:
            with state_lock:
                slot = pending.get(request.get("id"))
                if (slot is not None and not slot["done"] and not slot["cancelled"].is_set()
                        and not stopping.is_set() and time.monotonic() < slot["deadline"]):
                    try:
                        if "error" in request:
                            raise RpcError(request["error"]["code"], request["error"]["message"])
                        accept_result = slot["accept_result"]
                        if accept_result is not None and slot["method"] == "bus/subscribe":
                            observe_wire("subscribe_ack")
                        slot["result"] = (request["result"] if accept_result is None
                                          else accept_result(request["result"]))
                    except Exception as error:
                        slot["error"] = error
                    slot["done"] = True
                    state_lock.notify_all()
finally:
    stopping.set()
    bus.close()
    commands.put_nowait(None)
    with state_lock:
        state_lock.notify_all()
    tool_worker.join(timeout=1)
