/** Host-owned editor/suggestion handoff for Pi 0.84.4. No terminal ownership. */
export const EDITOR_HANDOFF_LIMITS = Object.freeze({
  textBytes: 256 * 1024, pendingRequests: 32, providers: 16,
  concurrentCompletions: 4, items: 32, completionTextBytes: 1024,
});

function unsupported(name) {
  throw new Error(`Pi compatibility API is not supported by octet: ${name}`);
}
function text(value, label, maxBytes = EDITOR_HANDOFF_LIMITS.textBytes, multiline = true) {
  if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > maxBytes
    || /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)
    || (multiline ? /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u : /[\x00-\x1f\x7f-\x9f]/u).test(value)) {
    throw new Error(`${label} must be bounded plain UTF-8 text`);
  }
  return value;
}
function revision(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("editor revision must be a portable unsigned integer");
  return value;
}
function completionText(value, label) {
  return text(value, label, EDITOR_HANDOFF_LIMITS.completionTextBytes, false);
}

export function createEditorHandoff({ request, isCurrent, diagnostic = () => {} }) {
  let disposed = false;
  let snapshot;
  let pending = 0;
  let operationChain = Promise.resolve();
  let registrationRevision = 0;
  let acceptedRevision = -1;
  let activeCompletions = 0;
  let querySequence = 0;
  const providers = [];
  const controller = new AbortController();
  const live = () => !disposed && isCurrent();
  function assertLive() {
    if (!live()) throw new Error("Pi editor handoff belongs to a stale UI owner");
  }
  function observe(value) {
    assertLive();
    if (!value || typeof value !== "object" || Array.isArray(value)
      || Object.keys(value).some((key) => !["text", "revision", "focused"].includes(key))
      || typeof value.focused !== "boolean") throw new Error("invalid host editor snapshot");
    const next = Object.freeze({ text: text(value.text, "editor text"), revision: revision(value.revision), focused: value.focused });
    // Delayed operation replies must never overwrite a newer host notification.
    if (!snapshot || next.revision > snapshot.revision) snapshot = next;
    return snapshot;
  }
  function enqueue(method, params, signal, accept) {
    assertLive();
    if (pending >= EDITOR_HANDOFF_LIMITS.pendingRequests) throw new Error("Pi editor request queue is full");
    pending += 1;
    const combined = signal ? AbortSignal.any([signal, controller.signal]) : controller.signal;
    const result = operationChain.then(async () => {
      assertLive();
      combined.throwIfAborted();
      const response = await request(method, params, { signal: combined, isCurrent: live });
      assertLive();
      combined.throwIfAborted();
      return accept(response);
    });
    operationChain = result.then(() => {}, () => {});
    // Pi setters are void. Attach a rejection handler even if the extension
    // ignores our optional acknowledgement promise; never fake host success.
    void result.catch((error) => { if (live() && !combined.aborted) diagnostic(error); });
    void result.finally(() => { pending -= 1; }).catch(() => {});
    return result;
  }
  function operate(params, signal) { return enqueue("ui/editor", params, signal, observe); }
  function register(signal) {
    const nextRevision = registrationRevision + 1;
    const result = enqueue("ui/autocomplete/register", { revision: nextRevision }, signal, (response) => {
      if (!response || typeof response.accepted !== "boolean"
        || Object.keys(response).some((key) => key !== "accepted")) throw new Error("invalid host autocomplete admission");
      if (!response.accepted) throw new Error("host declined Pi autocomplete registration");
      if (registrationRevision === nextRevision) acceptedRevision = nextRevision;
    });
    registrationRevision = nextRevision;
    return result;
  }
  function disposeProvider(entry) {
    if (entry.disposed) return;
    entry.disposed = true;
    try { entry.provider?.dispose?.(); } catch (error) { diagnostic(error); }
  }
  function suggestionsBefore(entry, args) {
    assertLive();
    const index = providers.indexOf(entry);
    const previous = index > 0 ? providers[index - 1] : null;
    return previous ? previous.provider.getSuggestions(...args) : null;
  }
  function addAutocompleteProvider(factory, signal) {
    assertLive();
    if (typeof factory !== "function") throw new Error("Pi autocomplete requires a provider wrapper factory");
    if (providers.length >= EDITOR_HANDOFF_LIMITS.providers) throw new Error("Pi autocomplete provider limit exceeded");
    const entry = { provider: null, disposed: false };
    const base = Object.freeze({
      getSuggestions: (...args) => suggestionsBefore(entry, args),
      // Arbitrary cursor rewrites cannot cross the semantic host boundary.
      applyCompletion: () => unsupported("autocomplete.applyCompletion (host-owned suffix replacement only)"),
    });
    const provider = factory(base);
    entry.provider = provider;
    if (!provider || typeof provider !== "object" || typeof provider.then === "function"
      || typeof provider.getSuggestions !== "function") {
      disposeProvider(entry);
      throw new Error("Pi autocomplete factory must return a synchronous provider object");
    }
    let admitted;
    try { admitted = register(signal); }
    catch (error) { disposeProvider(entry); throw error; }
    providers.push(entry);
    void admitted.catch(() => {
      const index = providers.indexOf(entry);
      if (index >= 0) providers.splice(index, 1);
      querySequence += 1;
      disposeProvider(entry);
    });
    return () => {
      if (entry.disposed) return;
      const index = providers.indexOf(entry);
      if (index >= 0) providers.splice(index, 1);
      disposeProvider(entry);
      querySequence += 1;
      if (live()) void register(signal);
    };
  }
  async function complete(params, signal) {
    assertLive();
    if (!params || typeof params !== "object" || Array.isArray(params)
      || Object.keys(params).some((key) => !["text", "cursor", "revision"].includes(key))) throw new Error("invalid autocomplete request");
    const value = text(params.text, "autocomplete editor text");
    const hostRevision = revision(params.revision);
    const bytes = Buffer.from(value, "utf8");
    const cursor = params.cursor;
    if (!Number.isSafeInteger(cursor) || cursor < 0 || cursor > bytes.length
      || (cursor < bytes.length && (bytes[cursor] & 0xc0) === 0x80)) throw new Error("autocomplete cursor must be a UTF-8 boundary");
    const empty = () => ({ prefix: "", items: [] });
    if (!providers.length || acceptedRevision !== registrationRevision
      || (snapshot && hostRevision < snapshot.revision)) return empty();
    if (activeCompletions >= EDITOR_HANDOFF_LIMITS.concurrentCompletions) throw new Error("Pi autocomplete query limit exceeded");
    const sequence = ++querySequence;
    const selectedRevision = registrationRevision;
    const before = bytes.subarray(0, cursor).toString("utf8");
    const beforeLines = before.split("\n");
    const combined = signal ? AbortSignal.any([signal, controller.signal]) : controller.signal;
    combined.throwIfAborted();
    activeCompletions += 1;
    let onAbort;
    try {
      const cancelled = new Promise((_, reject) => {
        onAbort = () => reject(combined.reason);
        combined.addEventListener("abort", onAbort, { once: true });
      });
      const provider = providers.at(-1).provider;
      const computation = Promise.resolve().then(() => {
        combined.throwIfAborted();
        return provider.getSuggestions(value.split("\n"), beforeLines.length - 1, beforeLines.at(-1).length);
      });
      // A cancelled non-cooperative provider still occupies a bounded slot
      // until it settles. Repeated cancellation cannot spawn unlimited work.
      void computation.then(() => { activeCompletions -= 1; }, () => { activeCompletions -= 1; });
      const result = await Promise.race([computation, cancelled]);
      combined.throwIfAborted();
      if (!live() || sequence !== querySequence || selectedRevision !== registrationRevision
        || (snapshot && hostRevision < snapshot.revision) || result == null) return empty();
      const prefix = completionText(result.prefix, "autocomplete prefix");
      if (!before.endsWith(prefix)) throw new Error("autocomplete prefix must be the exact suffix before the cursor");
      if (!Array.isArray(result.items) || result.items.length > EDITOR_HANDOFF_LIMITS.items) throw new Error("autocomplete items exceed the bounded host contract");
      const items = result.items.map((item) => {
        if (!item || typeof item !== "object") throw new Error("invalid autocomplete item");
        return {
          value: completionText(item.value, "autocomplete value"),
          label: completionText(item.label, "autocomplete label"),
          ...(item.description == null ? {} : { description: completionText(item.description, "autocomplete description") }),
        };
      });
      return { prefix, items };
    } finally {
      combined.removeEventListener("abort", onAbort);
    }
  }
  return Object.freeze({
    observe, complete, addAutocompleteProvider,
    refresh: (signal) => operate({ operation: "get" }, signal),
    setEditorText: (value, signal) => operate({ operation: "set", text: text(value, "editor text") }, signal),
    pasteToEditor: (value, signal) => operate({ operation: "paste", text: text(value, "editor paste") }, signal),
    focus: (signal) => operate({ operation: "focus" }, signal),
    getEditorText() {
      assertLive();
      if (!snapshot) throw new Error("host editor snapshot is not available yet");
      return snapshot.text;
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      controller.abort(new Error("Pi editor owner disposed"));
      for (const entry of providers.splice(0)) disposeProvider(entry);
      snapshot = undefined;
    },
  });
}
