// Pi 1.0.2 extension fixture for scripts/test-pi-api-session.py.
//
// Uses only the public Pi extension API (docs/pi-extension-api.md rows 1-4) and
// is loaded unchanged through the octet-pi-compat adapter. The harness owns
// every expectation; this file only performs the Pi calls under test.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { resolve } from "node:path";

export default function piApiSession(pi: ExtensionAPI) {
  // Row 1: tool_call handlers mutate event.input in place; the tool runs the
  // final arguments.
  pi.on("tool_call", (event, ctx) => {
    if (event.toolName !== "read") return;
    if (event.input.path === "original.txt" || event.input.path === "../policy-enter.txt") {
      event.input.path = "changed.txt";
    } else if (event.input.path === "policy-original.txt") {
      // The harness runs this case under controlled policy. Admission must
      // inspect this final outside-workspace path, not the harmless original.
      event.input.path = resolve(ctx.cwd, "..", "policy-denied.txt");
    }
  });

  // Row 2: tool_result handlers replace the content the model receives. The
  // replacement keeps the text the handler saw and appends a marker naming the
  // input it was given (Pi passes the same, mutated, argument object).
  pi.on("tool_result", (event) => {
    if (event.toolName !== "read") return;
    const seen = event.content.map((part) => (part.type === "text" ? part.text : "")).join("");
    const replacement = {
      content: [{ type: "text" as const, text: `${seen} [replaced input.path=${String(event.input.path)} seenError=${event.isError}]` }],
    };
    // A hook must not turn a native policy denial into an admitted success.
    // Ordinary execution failures keep isError by omitting the field.
    return String(event.input.path).endsWith("/policy-denied.txt")
      ? { ...replacement, isError: false }
      : replacement;
  });

  // Row 3: an idle custom message that triggers a turn. display:false hides it
  // from the transcript; details stay out of the model request.
  pi.registerCommand("probe-message", {
    description: "Send a hidden custom message that triggers a turn",
    handler: async () => {
      pi.sendMessage(
        {
          customType: "probe",
          content: "custom-probe",
          display: false,
          details: { n: 1, sentinel: "probe-details-sentinel" },
        },
        { triggerTurn: true },
      );
    },
  });

  // Row 4: replace the session from a command and act through the fresh context.
  pi.registerCommand("probe-new", {
    description: "Start a new session and write into it through withSession",
    handler: async (_args, ctx) => {
      await ctx.newSession({
        withSession: async (fresh) => {
          const sessionId = fresh.sessionManager.getSessionId();
          fresh.ui.notify(`fresh ${sessionId}`);
          // Both awaits cross the real adapter/host RPC boundary. Reading the
          // fresh mirror after the write must still name the replacement owner.
          await fresh.sendMessage({ customType: "probe", content: "in-new-session", display: true,
            details: { sessionId, sentinel: "fresh-details-sentinel" } });
          await fresh.sendMessage({ customType: "probe", content: "after-fresh-rpc", display: true,
            details: { sessionId: fresh.sessionManager.getSessionId() } });
          fresh.ui.notify(`fresh-rpc-complete ${fresh.sessionManager.getSessionId()}`);
        },
      });
    },
  });
}
