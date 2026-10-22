// Path B overlay (see ../installed-pi.mjs). Only reachable when the runtime
// was explicitly started with pi_runtime "installed".
const namespace = globalThis[Symbol.for('octet.pi-compat.installed-pi')];
if (typeof namespace !== 'function') throw new Error('installed-Pi overlay loaded without an explicit pi_runtime "installed" runtime');
module.exports = namespace('coding-agent');

// Advertise existing constructors to Node's CommonJS named-export lexer.
// Otherwise jiti finds them only through the default namespace and binds them,
// losing their prototypes. The guarded namespace still owns every export grant.
if ('ArminComponent' in module.exports) module.exports.ArminComponent = module.exports.ArminComponent;
if ('AssistantMessageComponent' in module.exports) module.exports.AssistantMessageComponent = module.exports.AssistantMessageComponent;
if ('BashExecutionComponent' in module.exports) module.exports.BashExecutionComponent = module.exports.BashExecutionComponent;
if ('BranchSummaryMessageComponent' in module.exports) module.exports.BranchSummaryMessageComponent = module.exports.BranchSummaryMessageComponent;
if ('CompactionSummaryMessageComponent' in module.exports) module.exports.CompactionSummaryMessageComponent = module.exports.CompactionSummaryMessageComponent;
if ('CredentialSynchronizationError' in module.exports) module.exports.CredentialSynchronizationError = module.exports.CredentialSynchronizationError;
if ('CustomMessageComponent' in module.exports) module.exports.CustomMessageComponent = module.exports.CustomMessageComponent;
if ('ExtensionEditorComponent' in module.exports) module.exports.ExtensionEditorComponent = module.exports.ExtensionEditorComponent;
if ('ExtensionInputComponent' in module.exports) module.exports.ExtensionInputComponent = module.exports.ExtensionInputComponent;
if ('ExtensionSelectorComponent' in module.exports) module.exports.ExtensionSelectorComponent = module.exports.ExtensionSelectorComponent;
if ('FooterComponent' in module.exports) module.exports.FooterComponent = module.exports.FooterComponent;
if ('ModelSelectorComponent' in module.exports) module.exports.ModelSelectorComponent = module.exports.ModelSelectorComponent;
if ('SessionSelectorComponent' in module.exports) module.exports.SessionSelectorComponent = module.exports.SessionSelectorComponent;
if ('SettingsSelectorComponent' in module.exports) module.exports.SettingsSelectorComponent = module.exports.SettingsSelectorComponent;
if ('ShowImagesSelectorComponent' in module.exports) module.exports.ShowImagesSelectorComponent = module.exports.ShowImagesSelectorComponent;
if ('SkillInvocationMessageComponent' in module.exports) module.exports.SkillInvocationMessageComponent = module.exports.SkillInvocationMessageComponent;
if ('Theme' in module.exports) module.exports.Theme = module.exports.Theme;
if ('ThemeSelectorComponent' in module.exports) module.exports.ThemeSelectorComponent = module.exports.ThemeSelectorComponent;
if ('ThinkingSelectorComponent' in module.exports) module.exports.ThinkingSelectorComponent = module.exports.ThinkingSelectorComponent;
if ('ToolExecutionComponent' in module.exports) module.exports.ToolExecutionComponent = module.exports.ToolExecutionComponent;
if ('TreeSelectorComponent' in module.exports) module.exports.TreeSelectorComponent = module.exports.TreeSelectorComponent;
if ('UserMessageComponent' in module.exports) module.exports.UserMessageComponent = module.exports.UserMessageComponent;
if ('UserMessageSelectorComponent' in module.exports) module.exports.UserMessageSelectorComponent = module.exports.UserMessageSelectorComponent;
if ('AgentSession' in module.exports) module.exports.AgentSession = module.exports.AgentSession;
if ('AuthStorage' in module.exports) module.exports.AuthStorage = module.exports.AuthStorage;
if ('BorderedLoader' in module.exports) module.exports.BorderedLoader = module.exports.BorderedLoader;
if ('CustomEditor' in module.exports) module.exports.CustomEditor = module.exports.CustomEditor;
if ('DefaultResourceLoader' in module.exports) module.exports.DefaultResourceLoader = module.exports.DefaultResourceLoader;
if ('DynamicBorder' in module.exports) module.exports.DynamicBorder = module.exports.DynamicBorder;
if ('ModelRegistry' in module.exports) module.exports.ModelRegistry = module.exports.ModelRegistry;
if ('SessionManager' in module.exports) module.exports.SessionManager = module.exports.SessionManager;
if ('SettingsManager' in module.exports) module.exports.SettingsManager = module.exports.SettingsManager;
