// Path B overlay (see ../installed-pi.mjs). Only reachable when the runtime
// was explicitly started with pi_runtime "installed".
const namespace = globalThis[Symbol.for('octet.pi-compat.installed-pi')];
if (typeof namespace !== 'function') throw new Error('installed-Pi overlay loaded without an explicit pi_runtime "installed" runtime');
module.exports = namespace('ai');

// Advertise existing constructors to Node's CommonJS named-export lexer.
// Otherwise jiti finds them only through the default namespace and binds them,
// losing their prototypes. The guarded namespace still owns every export grant.
if ('AssistantMessageEventStream' in module.exports) module.exports.AssistantMessageEventStream = module.exports.AssistantMessageEventStream;
if ('AssistantMessageFrameEncoder' in module.exports) module.exports.AssistantMessageFrameEncoder = module.exports.AssistantMessageFrameEncoder;
if ('EventStream' in module.exports) module.exports.EventStream = module.exports.EventStream;
if ('ModelsError' in module.exports) module.exports.ModelsError = module.exports.ModelsError;
