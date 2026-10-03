export declare const API_VERSION: '0.4';
export declare class CancelledError extends Error { constructor(); }
/** Explicit model-visible tool failure; unexpected exceptions stay private. */
export declare class ToolError extends Error { constructor(message: string); }
export declare class UnsupportedFeatureError extends Error { constructor(feature: string); }

export type Scalar = string | number | boolean | null;
export interface Schema {
  readonly type: 'object' | 'array' | 'string' | 'number' | 'integer' | 'boolean' | 'null';
  readonly properties?: Readonly<Record<string, Schema>>;
  readonly required?: readonly string[];
  readonly additionalProperties?: boolean;
  readonly items?: Schema;
  readonly enum?: readonly Scalar[];
  readonly description?: string;
  readonly title?: string;
  readonly minimum?: number;
  readonly maximum?: number;
  readonly minLength?: number;
  readonly maxLength?: number;
  readonly minItems?: number;
  readonly maxItems?: number;
}
type RequiredKeys<S> = S extends { readonly required: readonly (infer K)[] } ? K : never;
export type InferSchema<S> = S extends { readonly enum: readonly (infer V)[] } ? V :
  S extends { readonly type: 'object'; readonly properties: infer P } ?
    { -readonly [K in keyof P as K extends RequiredKeys<S> ? K : never]-?: InferSchema<P[K]> } &
    { -readonly [K in keyof P as K extends RequiredKeys<S> ? never : K]?: InferSchema<P[K]> } :
  S extends { readonly type: 'array'; readonly items: infer I } ? InferSchema<I>[] :
  S extends { readonly type: 'string' } ? string :
  S extends { readonly type: 'integer' | 'number' } ? number :
  S extends { readonly type: 'boolean' } ? boolean :
  S extends { readonly type: 'null' } ? null : Record<string, unknown>;

export interface ResourceOwner {
  readonly session_id: string;
  readonly extension_instance_id: string;
  readonly process_generation: number;
}
export interface RequestContext {
  readonly workspace: string;
  readonly execution_scope?: string | null;
  readonly host: Readonly<Record<string, unknown>>;
  readonly resource_owner?: ResourceOwner | null;
  readonly signal: AbortSignal;
  readonly supportsProgress: boolean;
  throwIfCancelled(): void;
  /** Cooperative, abortable delay. Long handlers must yield between effects. */
  sleep(ms: number): Promise<void>;
  /** Ephemeral status; throws UnsupportedFeatureError if not offered by the host. */
  progress(message: string, counters?: {current?: number; total?: number; unit?: string}): Promise<void>;
}
export interface ToolResult { text: string; isError?: boolean; }
export interface ToolDefinition<S extends Schema = Schema> {
  name: string;
  description: string;
  parameters: S & {readonly type: 'object'};
}
export interface CommandDefinition { name: string; description: string; usage?: string; }
export declare class Extension {
  constructor(options?: {maxConcurrentRequests?: number});
  tool<const S extends Schema>(definition: ToolDefinition<S>, handler:
    (arguments_: InferSchema<S>, context: RequestContext) => string | ToolResult | Promise<string | ToolResult>): this;
  command(definition: CommandDefinition, handler:
    (arguments_: string[], context: RequestContext) => string | Promise<string>): this;
  /** Hooks are deliberately unsupported, never silently replaced with no-ops. */
  hook(...arguments_: unknown[]): never;
  onShutdown(handler: (context: {readonly reason: 'shutdown' | 'transport_lost'}) => void | Promise<void>): this;
  contributions(): {tools: string[]; commands: string[]};
  /** Owns stdio and exits the process at its bounded terminal boundary. */
  run(): void;
}
