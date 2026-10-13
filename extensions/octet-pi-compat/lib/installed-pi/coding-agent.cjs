// Path B overlay (see ../installed-pi.mjs). Only reachable when the runtime
// was explicitly started with pi_runtime "installed".
const namespace = globalThis[Symbol.for('octet.pi-compat.installed-pi')];
if (typeof namespace !== 'function') throw new Error('installed-Pi overlay loaded without an explicit pi_runtime "installed" runtime');
module.exports = namespace('coding-agent');
