# octet v0.10.0 — Pi side-by-side evidence

Real Pi 1.4.0 (left) and the recorded octet release candidate (right) running the
same scripted user journey, setup S01 ("maintainer": the user's existing Pi
extensions, skills, prompts, themes and codemode preference).

- How it was run: [METHODOLOGY.md](METHODOLOGY.md). Two isolated Linux
  displays in one Cua Space, one real terminal each, joined side by side. Both
  sides use the same loopback mock model provider; no real inference.
- What was run: [PROVENANCE.txt](PROVENANCE.txt). Source HEAD `33fd1186d`, its
  recorded dirty-source identity, and binary hashes. These captures do not
  qualify later candidate revisions.
- Frames: [s01-side-by-side/](s01-side-by-side/), 22 frames from startup through
  slash menu, `/goal`, model picker, thinking, Markdown/Mermaid replies, long
  scrollback, tool and codemode calls, steering, abort, `/reload`, exit and
  resume instructions and shell restoration probes. No resumed-agent frame is
  included.

Expected differences are documented in
[docs/releases/v0.10.0.md](../../docs/releases/v0.10.0.md) (Known
limitations) and
[docs/pi-compatibility.md](../../docs/pi-compatibility.md#pi-packages-octet-replaces).
Visible here: octet's built-in `/goal` (frame 04) replaces `@narumitw/pi-goal`
and handles `/goal` where Pi sends it to the model as text. The `/reload`
summary (frame 19) differs in what it reports as reloaded.

Qualification remains incomplete: frame 14 shows a Hermes auto-review warning on
the octet side (`provider_credentials` not negotiated and `spawn pi ENOENT`);
frame 16 shows a Hermes warning on the Pi side (`parse_error` and `spawn pi ENOENT`).
The launcher correction described in METHODOLOGY must be reconciled with these
visible errors before treating the captures as the corrected canonical run.
The scripted assistant confirmations alone do not verify successful tool execution.
S01 is one setup, not evidence of parity across multiple setups or themes.
