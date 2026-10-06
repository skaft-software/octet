// Conversion bounds are request-local. Legacy peers retain their exact profile.
import { AsyncLocalStorage } from 'node:async_hooks';
const limits = new AsyncLocalStorage();
export const withContextLimits = (profile, work) => limits.run(profile, work);
export const contextBytes = () => limits.getStore()?.projection_bytes ?? 786432;
export const contextItems = () => limits.getStore() ? Infinity : 8192;
export const historyItems = () => limits.getStore()?.view_entries ?? 16384;
