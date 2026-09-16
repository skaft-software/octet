// This fixture runs through the fake public loader and the actual bridge.
export default function uiLifecycleFixture(pi) {
  let componentDisposals = 0;
  let providerDisposals = 0;
  let oldStatus;
  let oldRender;
  let removeProvider;
  let releaseSuggestions;

  const register = (name, handler) => pi.registerCommand(name, {
    description: `UI lifecycle fixture ${name}`, handler,
  });
  function install(ctx) {
    const ui = ctx.ui;
    oldStatus = () => ui.setStatus("late-status", "must-not-escape");
    ctx.ui.setStatus("fixture-status", "active");
    ctx.ui.setHeader((tui) => {
      oldRender = () => tui.requestRender();
      return {
        render: (width) => [`header-${width}`],
        invalidate() {},
        dispose() { componentDisposals += 1; },
      };
    });
    removeProvider = ctx.ui.addAutocompleteProvider(() => ({
      getSuggestions(lines, row, column) {
        const prefix = lines[row].slice(0, column);
        if (prefix === "hold") {
          return new Promise((resolve) => {
            releaseSuggestions = resolve;
            ctx.ui.notify("suggestions-held");
          });
        }
        return { prefix, items: [{ value: `${prefix}-complete`, label: "fixture choice" }] };
      },
      dispose() { providerDisposals += 1; },
    }));
  }
  register("ui-install", async (_args, ctx) => {
    install(ctx);
    await ctx.ui.setEditorText("seed");
    if (ctx.ui.getEditorText() !== "seed") throw new Error("editor acknowledgement was not observed");
  });
  register("ui-install-and-wait", async (_args, ctx) => {
    install(ctx);
    await ctx.ui.input("hold installing command");
  });
  register("ui-editor-wait", async (_args, ctx) => {
    await ctx.ui.setEditorText("held-write");
  });
  register("ui-report", async (_args, ctx) => {
    let statusRejected = false;
    let renderRejected = false;
    try { oldStatus(); } catch (error) { statusRejected = /stale|disposed/.test(String(error)); }
    try { oldRender(); } catch (error) { renderRejected = /stale|disposed/.test(String(error)); }
    ctx.ui.notify(JSON.stringify({ componentDisposals, providerDisposals, statusRejected, renderRejected }));
  });
  register("ui-remove-provider", async () => {
    removeProvider();
    removeProvider();
  });
  register("ui-release-suggestions", async () => {
    if (!releaseSuggestions) throw new Error("no held suggestion query");
    releaseSuggestions({ prefix: "hold", items: [{ value: "late", label: "late" }] });
  });
  register("ui-editor-read", async (_args, ctx) => {
    ctx.ui.notify(ctx.ui.getEditorText());
  });
  for (const method of ["confirm", "input", "select"]) {
    for (const mode of ["timeout", "abort", "pre-abort", "wait", "reply"]) {
      register(`ui-dialog-${method}-${mode}`, async (_args, ctx) => {
        const controller = new AbortController();
        if (mode === "pre-abort") controller.abort();
        const opts = { signal: controller.signal, ...(mode === "timeout" ? { timeout: 30 } : {}) };
        const timer = mode === "abort" ? setTimeout(() => controller.abort(), 30) : undefined;
        try {
          const value = await ctx.ui[method]("fixture dialog", method === "select" ? ["first", "1st", "second"] : "detail", opts);
          ctx.ui.notify(JSON.stringify({ method, value: value ?? null }));
        } finally { clearTimeout(timer); }
      });
    }
  }
  register("ui-wait-idle", async (_args, ctx) => {
    await ctx.waitForIdle();
    ctx.ui.notify("idle-confirmed");
  });
  register("ui-widget", async (_args, ctx) => {
    ctx.ui.setWidget("fixture-widget", ["visible"]);
  });
}
