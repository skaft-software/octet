import { Type } from 'typebox';
import { unsupported } from '../lib/errors.mjs';
export { Type };
export { uuidv7 } from '../lib/uuid.mjs';
export { calculateCost, collapseSystemMessages, createAssistantMessageEventStream,
  getCurrentSystemPrompt, getCurrentTools } from '../lib/ai-public-helpers.mjs';
// Pi 1.0.2 packages/ai/src/utils/typebox-helpers.ts, verbatim in behaviour.
export const StringEnum = (values, options) => Type.Unsafe({
  type: 'string', enum: values,
  ...(options?.description && { description: options.description }),
  ...(options?.default && { default: options.default }),
});
export const getModel = () => unsupported('pi-ai.getModel', 'model selection and inventory remain host-owned');
export const getModels = () => unsupported('pi-ai.getModels', 'model inventory was not supplied by octet');
export const complete = () => unsupported('pi-ai.complete', 'Rust owns inference; no Pi provider runtime is loaded');
export const stream = () => unsupported('pi-ai.stream', 'Rust owns inference');
export const completeSimple = complete;
export const streamSimple = stream;
