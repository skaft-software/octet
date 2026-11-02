// Readiness probe for the Pi extensions eval. It is deliberately tiny: it
// performs no work of its own, so the time at which PI-EVAL-READY reaches the
// screen reflects how long the *host* (Pi or octet) needed to get every
// configured extension live and deliver session_start.
//
// The marker goes to two surfaces because footer replacements (for example
// pi-powerline-footer) decide for themselves whether to draw extension
// statuses; the widget above the editor is always drawn.
export default function (pi: any) {
  pi.on("session_start", async (_event: unknown, ctx: any) => {
    try {
      ctx.ui.setStatus("pi-eval", "PI-EVAL-READY");
    } catch {}
    try {
      ctx.ui.setWidget("pi-eval", ["PI-EVAL-READY"]);
    } catch {}
  });
}
