// Stands in for Pi modules octet does not emulate (pi-agent-core, pi-ai/oauth,
// pi-ai/providers/all). Loading it fails, so the extension is retried against
// the installed Pi 1.0.2, which provides the real module.
import { rpcError } from '../lib/errors.mjs';
import { FALLBACK_ELIGIBLE } from '../lib/installed-pi.mjs';

throw rpcError(FALLBACK_ELIGIBLE, 'pi_compat_fallback_eligible this Pi module is provided only by the installed Pi 1.0.2');
