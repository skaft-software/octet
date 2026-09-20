---
name: Pi Migration Cleanup
description: Use octet's zero-token Pi inventory and organize only the remaining compatibility decisions.
version: 0.1.0
required-tools:
  - read
  - bash
tags:
  - pi
  - migration
  - compatibility
---
# Pi Migration Cleanup

Use this procedure only after explicit activation. Keep the model portion small;
the host scanner, not the model, owns discovery and classification.

1. Run `octet migrate pi --dry-run --summary` first. Request `--json` only when a
   package needs targeted inspection.
2. Never read Pi credential/model stores, install dependencies, execute Pi
   package code, or send setup contents to a network service.
3. Treat `direct` resources as portable candidates requiring review. `bridge`
   is a historical scanner label, not an execution path. Pi extensions cannot
   run unchanged; `native_port`/`manual`/`blocked` need explicit residual work.
4. Preview portable setup import with `octet migrate import pi --dry-run`.
   Apply only after explicit user approval; never infer permission to install,
   enable, trust, or execute code from an inventory result.
5. Report source paths, classifications, unsupported surfaces, and the next
   bounded review or porting step. Keep native provider configuration separate
   from Pi extension compatibility. A healthy octet process is not Pi parity.
