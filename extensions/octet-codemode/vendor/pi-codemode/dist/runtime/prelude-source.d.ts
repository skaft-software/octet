/**
 * JavaScript evaluated inside the QuickJS VM before the script runs.
 *
 * The VM is its own wasm instance, so nothing here guards a realm boundary.
 * The prelude keeps the host bridge in a closure so the script cannot call it
 * directly, and builds `tools`, `ALL_TOOLS`, the output helpers (`text`, `image`,
 * `exit`, `console`), and globals on top of it. Tool arguments and results cross
 * as JSON strings and are parsed on this side.
 *
 * Output helpers: `text(value)` appends a text item (non-strings are
 * JSON-stringified), `image(urlOrItem)` appends an image from a base64 `data:` URL, an
 * `{ image_url }` object, or an MCP `ImageContent` block, and `exit()` ends the script
 * successfully. `console.*` appends text items like `text()`.
 *
 * `store(key, value)` and `load(key)` are synchronous: they work on a snapshot of
 * JSON text passed in as `storeJson`, and the keys the script wrote are reported
 * with a successful "done".
 *
 * Evaluates to a function `(bridge, toolsJson, globalsJson, storeJson) => { settle, run, stalled }`.
 * `toolsJson` lists `{ name, jsName, description }`: `tools[jsName]` and `tools[name]` call the
 * tool, and `ALL_TOOLS` lists `{ name: jsName, description }`.
 * `stalled()` reports a script that has not finished while no host call is pending: with no timers
 * or I/O in the VM, nothing can ever resume it.
 * `globalsJson` lists `{ name, spread }`; `a.b` names are grouped into a frozen `a` object.
 * `bridge(kind, a, b, c)` with kind "call" or "global" (id, name, argsJson),
 * "output" ("text", text) or ("image", data, mimeType), or "done" (ok, valueJsonOrErrorJson, writesJson).
 */
export declare const MAX_STORE_VALUE_CHARS: number;
export declare const MAX_STORE_TOTAL_CHARS: number;
export declare const PRELUDE_SOURCE: string;
//# sourceMappingURL=prelude-source.d.ts.map