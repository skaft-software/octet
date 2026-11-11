import { Type } from 'typebox';
import inventory from './pi-1.0.2/models.json' with { type: 'json' };
import { unsupported } from '../lib/errors.mjs';
export { Type };
export { uuidv7 } from '../lib/uuid.mjs';
export * from './pi-1.0.2/ai/text.mjs';
export * from './pi-1.0.2/ai/transcript.mjs';
export * from './pi-1.0.2/ai/event-stream.mjs';
export * from './pi-1.0.2/ai/models.mjs';
export * from './pi-1.0.2/ai/retry.mjs';
export * from './pi-1.0.2/ai/overflow.mjs';
export * from './pi-1.0.2/ai/diagnostics.mjs';
import * as localValidation from './pi-1.5.0/ai/validation.mjs';
import { helper, helperClass, refused } from './local-pi.mjs';
// Pi 1.0.2 packages/ai/src/utils/typebox-helpers.ts, verbatim in behaviour.
export const StringEnum = (values, options) => Type.Unsafe({
  type: 'string', enum: values,
  ...(options?.description && { description: options.description }),
  ...(options?.default && { default: options.default }),
});
export const getModel = (provider, id) => inventory.MODELS[provider]?.[id];
export const getModels = provider => Object.values(inventory.MODELS[provider] ?? {});
export const getProviders = () => Object.keys(inventory.MODELS);
export const complete = () => unsupported('pi-ai.complete', 'Rust owns inference; no Pi provider runtime is loaded');
export const stream = () => unsupported('pi-ai.stream', 'Rust owns inference');
export const completeSimple = complete;
export const streamSimple = stream;
// Explicit provider streams an extension calls for its own provider run the
// user's installed Pi implementation unchanged (Wine-style): the request a
// Pi package builds is the request Pi builds, with the extension's own key.
export const streamAnthropic = helper('ai', 'streamAnthropic');
export const streamSimpleAnthropic = helper('ai', 'streamSimpleAnthropic');
export const streamAzureOpenAIResponses = helper('ai', 'streamAzureOpenAIResponses');
export const streamSimpleAzureOpenAIResponses = helper('ai', 'streamSimpleAzureOpenAIResponses');
export const streamGoogle = helper('ai', 'streamGoogle');
export const streamSimpleGoogle = helper('ai', 'streamSimpleGoogle');
export const streamGoogleVertex = helper('ai', 'streamGoogleVertex');
export const streamSimpleGoogleVertex = helper('ai', 'streamSimpleGoogleVertex');
export const streamMistral = helper('ai', 'streamMistral');
export const streamSimpleMistral = helper('ai', 'streamSimpleMistral');
export const streamOpenAICodexResponses = helper('ai', 'streamOpenAICodexResponses');
export const streamSimpleOpenAICodexResponses = helper('ai', 'streamSimpleOpenAICodexResponses');
export const streamOpenAICompletions = helper('ai', 'streamOpenAICompletions');
export const streamSimpleOpenAICompletions = helper('ai', 'streamSimpleOpenAICompletions');
export const streamOpenAIResponses = helper('ai', 'streamOpenAIResponses');
export const streamSimpleOpenAIResponses = helper('ai', 'streamSimpleOpenAIResponses');

export { anthropicMessagesApi } from '../lib/native-provider.mjs';
// Tool-call argument parsing and validation moved with Pi 1.5 (pi-schema
// `parse()`, `argumentsError` short-circuit, dotted error paths). The manifest
// routes both to the installed release when one answers, so a 1.5 setup validates
// exactly as Pi 1.5 does; the local port is the emulated-path fallback.
export const validateToolArguments = helper('ai', 'validateToolArguments', localValidation.validateToolArguments);
export const validateToolCall = helper('ai', 'validateToolCall', localValidation.validateToolCall);
export const parseToolCallArguments = helper('ai', 'parseToolCallArguments');
export const parseJsonWithRepair = helper('ai', 'parseJsonWithRepair');
export const parseStreamingJson = helper('ai', 'parseStreamingJson');
export const repairJson = helper('ai', 'repairJson');
export const reduceAssistantMessageFrames = helper('ai', 'reduceAssistantMessageFrames');
export const AssistantMessageFrameEncoder = helperClass('ai', 'AssistantMessageFrameEncoder');
export const createFauxCore = helper('ai', 'createFauxCore');
export const fauxProvider = helper('ai', 'fauxProvider');
export const fauxAssistantMessage = helper('ai', 'fauxAssistantMessage');
export const fauxText = helper('ai', 'fauxText');
export const fauxThinking = helper('ai', 'fauxThinking');
export const fauxToolCall = helper('ai', 'fauxToolCall');
export const cleanupSessionResources = helper('ai', 'cleanupSessionResources');
export const registerSessionResourceCleanup = helper('ai', 'registerSessionResourceCleanup');
// Pi 1.5.0 pi-ai/dist/utils/streaming-json.js, exported by dist/index.js. Providers that stream tool-call
// arguments (examples/extensions/custom-provider-anthropic/index.ts) import both at load time; the manifest
// routes them to the installed release (`added: 1.5.0`), so only the binding was missing.
export const StreamingJsonParser = helperClass('ai', 'StreamingJsonParser');
export const ToolCallArgumentsStream = helperClass('ai', 'ToolCallArgumentsStream');
export const InMemoryModelsStore = helperClass('ai', 'InMemoryModelsStore');
export const InMemoryCredentialStore = helperClass('ai', 'InMemoryCredentialStore');
export const ModelsError = helperClass('ai', 'ModelsError');
export const getImageModel = (provider, id) => inventory.IMAGE_MODELS[provider]?.[id];
export const getImageModels = provider => Object.values(inventory.IMAGE_MODELS[provider] ?? {});
export const getImageProviders = () => Object.keys(inventory.IMAGE_MODELS).filter(provider => getImageModels(provider).length);
export const ANTHROPIC_AUTH_TOKEN_ENV = 'ANTHROPIC_AUTH_TOKEN';
export const ANTHROPIC_OAUTH_TOKEN_ENV = 'ANTHROPIC_OAUTH_TOKEN';
export const ANTHROPIC_API_KEY_ENV = 'ANTHROPIC_API_KEY';
export const ANTHROPIC_FEDERATION_RULE_ID_ENV = 'ANTHROPIC_FEDERATION_RULE_ID';
export const ANTHROPIC_ORGANIZATION_ID_ENV = 'ANTHROPIC_ORGANIZATION_ID';
export const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV = 'ANTHROPIC_SERVICE_ACCOUNT_ID';
export const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV = 'ANTHROPIC_IDENTITY_TOKEN_FILE';
export const ANTHROPIC_WORKSPACE_ID_ENV = 'ANTHROPIC_WORKSPACE_ID';
export const azureOpenAIResponsesApi = refused('pi-ai.azureOpenAIResponsesApi');
export const bedrockConverseStreamApi = refused('pi-ai.bedrockConverseStreamApi');
export const createModels = refused('pi-ai.createModels');
export const createProvider = refused('pi-ai.createProvider');
export const defaultProviderAuthContext = refused('pi-ai.defaultProviderAuthContext');
export const findEnvKeys = helper('ai', 'findEnvKeys');
export const generateImages = refused('pi-ai.generateImages');
export const generateImagesOpenRouter = refused('pi-ai.generateImagesOpenRouter');
export const getApiProvider = refused('pi-ai.getApiProvider');
export const getApiProviders = refused('pi-ai.getApiProviders');
export const getEnvApiKey = helper('ai', 'getEnvApiKey');
export const getImagesApiProvider = refused('pi-ai.getImagesApiProvider');
export const googleGenerativeAIApi = refused('pi-ai.googleGenerativeAIApi');
export const googleVertexApi = refused('pi-ai.googleVertexApi');
export const mistralConversationsApi = refused('pi-ai.mistralConversationsApi');
export const openAICodexResponsesApi = refused('pi-ai.openAICodexResponsesApi');
export const openAICompletionsApi = refused('pi-ai.openAICompletionsApi');
export const openAIResponsesApi = refused('pi-ai.openAIResponsesApi');
export const piMessagesApi = refused('pi-ai.piMessagesApi');
export const registerApiProvider = refused('pi-ai.registerApiProvider');
export const registerBuiltInApiProviders = refused('pi-ai.registerBuiltInApiProviders');
export const registerBuiltInImagesApiProviders = refused('pi-ai.registerBuiltInImagesApiProviders');
export const registerFauxProvider = refused('pi-ai.registerFauxProvider');
export const registerImagesApiProvider = refused('pi-ai.registerImagesApiProvider');
export const resetApiProviders = refused('pi-ai.resetApiProviders');
export const setBedrockProviderModule = refused('pi-ai.setBedrockProviderModule');
export const unregisterApiProviders = refused('pi-ai.unregisterApiProviders');

export * from './pi-1.0.2/ai/lazy.mjs';
export * from './pi-1.0.2/ai/auth.mjs';
