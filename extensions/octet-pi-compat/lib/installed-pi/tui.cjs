// Path B overlay (see ../installed-pi.mjs). Only reachable when the runtime
// was explicitly started with pi_runtime "installed".
const namespace = globalThis[Symbol.for('octet.pi-compat.installed-pi')];
if (typeof namespace !== 'function') throw new Error('installed-Pi overlay loaded without an explicit pi_runtime "installed" runtime');
module.exports = namespace('tui');

// Advertise existing constructors to Node's CommonJS named-export lexer.
// Otherwise jiti finds them only through the default namespace and binds them,
// losing their prototypes. The guarded namespace still owns every export grant.
if ('CombinedAutocompleteProvider' in module.exports) module.exports.CombinedAutocompleteProvider = module.exports.CombinedAutocompleteProvider;
if ('HStack' in module.exports) module.exports.HStack = module.exports.HStack;
if ('Marked' in module.exports) module.exports.Marked = module.exports.Marked;
if ('MouseRegion' in module.exports) module.exports.MouseRegion = module.exports.MouseRegion;
if ('ScrollView' in module.exports) module.exports.ScrollView = module.exports.ScrollView;
if ('VStack' in module.exports) module.exports.VStack = module.exports.VStack;
if ('Box' in module.exports) module.exports.Box = module.exports.Box;
if ('CancellableLoader' in module.exports) module.exports.CancellableLoader = module.exports.CancellableLoader;
if ('Container' in module.exports) module.exports.Container = module.exports.Container;
if ('Editor' in module.exports) module.exports.Editor = module.exports.Editor;
if ('Image' in module.exports) module.exports.Image = module.exports.Image;
if ('Input' in module.exports) module.exports.Input = module.exports.Input;
if ('KeybindingsManager' in module.exports) module.exports.KeybindingsManager = module.exports.KeybindingsManager;
if ('Loader' in module.exports) module.exports.Loader = module.exports.Loader;
if ('Markdown' in module.exports) module.exports.Markdown = module.exports.Markdown;
if ('ProcessTerminal' in module.exports) module.exports.ProcessTerminal = module.exports.ProcessTerminal;
if ('SelectList' in module.exports) module.exports.SelectList = module.exports.SelectList;
if ('SettingsList' in module.exports) module.exports.SettingsList = module.exports.SettingsList;
if ('Spacer' in module.exports) module.exports.Spacer = module.exports.Spacer;
if ('TUI' in module.exports) module.exports.TUI = module.exports.TUI;
if ('Text' in module.exports) module.exports.Text = module.exports.Text;
if ('TruncatedText' in module.exports) module.exports.TruncatedText = module.exports.TruncatedText;
