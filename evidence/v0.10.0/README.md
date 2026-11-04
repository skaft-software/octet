# octet v0.10.0 — Pi side-by-side evidence

Real Pi 1.4.0 (left) and this release candidate of octet (right) running the
same scripted user journey, setup S01 ("maintainer": the user's existing Pi
extensions, skills, prompts, themes and codemode preference).

- How it was run: [METHODOLOGY.md](METHODOLOGY.md). Two isolated Linux
  displays in one Cua Space, one real terminal each, joined side by side. Both
  sides use the same loopback mock model provider; no real inference.
- What was run: [PROVENANCE.txt](PROVENANCE.txt). Candidate source identity and
  the binary hashes.
- Frames: [s01-side-by-side/](s01-side-by-side/), 22 frames from startup through
  slash menu, `/goal`, model picker, thinking, Markdown/Mermaid replies, long
  scrollback, tool and codemode calls, steering, abort, `/reload`, exit and
  resume.

Expected differences are documented in
[docs/releases/v0.10.0.md](../../docs/releases/v0.10.0.md) (Known
limitations) and
[docs/pi-compatibility.md](../../docs/pi-compatibility.md#pi-packages-octet-replaces).
Visible here: octet's built-in `/goal` (frame 04) replaces `@narumitw/pi-goal`
and handles `/goal` where Pi sends it to the model as text. The `/reload`
summary (frame 19) differs in what it reports as reloaded.
