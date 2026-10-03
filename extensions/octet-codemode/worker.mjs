// Trusted worker bootstrap around the *published* Pi worker, not a replacement
// VM/prelude. All bounds are on worker IPC; no Node authority enters QuickJS.
import { parentPort, workerData } from "node:worker_threads";
import { CAPTURE_BYTES, FRAME_BYTES, MAX_CALLS } from "./common.mjs";

const post = parentPort.postMessage.bind(parentPort);
let stopped = false;
let outputBytes = 0;
let outputParts = 0;
let images = 0;
let tools = 0;
let helpers = 0;
function fail(message) {
  stopped = true;
  post({ type: "crash", message });
  Atomics.store(new Int32Array(workerData.interrupt), 0, 1);
}
parentPort.postMessage = (message) => {
  if (stopped) return;
  if (message.type === "output") {
    outputParts++;
    outputBytes += Buffer.byteLength(message.item.type === "text" ? message.item.text : message.item.data);
    if (message.item.type === "image") images++;
    if (outputParts > 4096 || images > 64 || outputBytes > CAPTURE_BYTES) {
      fail("Script output capture limit exceeded (16 MiB, 4096 parts, 64 images); no store writes were committed");
      return;
    }
  } else if (message.type === "call") {
    if (message.target === "tool") tools++;
    else helpers++;
    if (tools > MAX_CALLS || helpers > 1024 || Buffer.byteLength(message.args ?? "") > FRAME_BYTES) {
      fail("Script bridge limit exceeded (256 tool calls, 1024 discovery calls, 1 MiB call arguments)");
      return;
    }
  } else if (message.type === "done") {
    if (Buffer.byteLength(message.value ?? message.error ?? "") + Buffer.byteLength(message.writes ?? "") > CAPTURE_BYTES) {
      fail("Script return/store serialization exceeded the 16 MiB capture limit");
      return;
    }
  }
  post(message);
};
await import("./vendor/pi-codemode/dist/runtime/worker.js");
