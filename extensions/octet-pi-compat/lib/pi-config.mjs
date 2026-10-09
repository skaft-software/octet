// Pi 1.0.2 `config.ts` package-location helpers and its session-format
// constant. Pi resolves these against the package that provides the
// pi-coding-agent module: on the emulated path that is this adapter package
// (which ships the same package.json/README.md layout the helpers name), and on
// the reviewed installed-Pi route the managed Pi release itself, exactly as
// real Pi resolves it there. Nothing is invented: `docs/` and `examples/` get
// no helper here because this package ships no such directories.
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { installedPiPackageDir } from './installed-pi.mjs';

export const getPackageDir = () => installedPiPackageDir() ?? dirname(dirname(fileURLToPath(import.meta.url)));
export const getPackageJsonPath = () => join(getPackageDir(), 'package.json');
export const getReadmePath = () => join(getPackageDir(), 'README.md');
// Pi 1.0.2 `core/session-manager.ts`. Octet's own session store keeps its own
// format; this is the Pi session-record version an extension reads/writes when
// it speaks Pi's session format directly.
export const CURRENT_SESSION_VERSION = 3;
