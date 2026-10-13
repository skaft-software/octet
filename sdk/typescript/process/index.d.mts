export declare const API_VERSION: '0.4';
export declare class CancelledError extends Error { constructor(); }
/** Explicit model-visible tool failure; unexpected exceptions stay private. */
export declare class ToolError extends Error { constructor(message: string); }
export declare class UnsupportedFeatureError extends Error { constructor(feature: string); }

export declare class HostRequestError extends Error {
  readonly code: number; readonly data?: unknown;
  constructor(code: number, message: string, data?: unknown);
}
export type ServiceFeature = 'artifacts' | 'policy_intents' | 'secrets' | 'composer' | 'session_entries' | 'message_injection' | 'active_tools' | 'bulk_objects_v1';
export type HostMethod = 'confirmation/request' | 'input/request' | 'artifact/publish' | 'policy/evaluate' | 'secret/get' |
  'composer/get' | 'composer/set' | 'composer/insert' | 'session/append_entry' | 'session/set_name' | 'session/set_label' |
  'session/send_message' | 'session/send_user_message' | 'tools/set_active' | 'resource/register' | 'resource/release' |
  'bulk/write' | 'bulk/commit' | 'bulk/read' | 'bulk/release';
export type HookName = 'before_prompt' | 'after_response' | 'before_tool_call' | 'after_tool_call' | 'provider_retry' |
  'before_persistence' | 'post_mutation' | 'cache_warming_decision' | 'compaction_strategy' | 'model_turn_start' | 'model_turn_end' |
  'session_before_compact' | 'session_compact' | 'session_before_tree' | 'session_tree';
export interface HookResult {
  disposition?: {action: 'continue'} | {action: 'deny'; reason: string};
  context?: {label: string; content: string; placement: 'system_prefix' | 'system_suffix' | 'prompt_prefix' | 'prompt_suffix'}[];
  notifications?: {level: 'info' | 'success' | 'warning' | 'error'; message: string; title?: string | null}[];
  provider_retry?: 'retry' | 'stop' | {delay: {additional_delay_ms: number}};
  persistence_metadata?: {value: unknown; public?: boolean};
  post_mutation?: {action: 'no_rescan'} | {action: 'request_rescan'; resource_ids: string[]};
  cache_warming_decision?: 'warm' | 'stop' | null;
  compaction_frames?: string[];
  session_operation?: {action: 'continue' | 'cancel'} | {action: 'replace_compaction'; replacement: {summary: string; first_kept: string}};
}
export type DiagnosticSource = {kind: 'workspace'; path: string; revision: string} | {kind: 'blob' | 'artifact'; id: string};
export interface DiagnosticLocation {source: DiagnosticSource; span: {start_byte: number; end_byte: number};}
export interface Diagnostic {
  severity: 'error' | 'warning' | 'info' | 'hint'; code: string; message: string;
  primary?: DiagnosticLocation;
  related?: {message: string; location: DiagnosticLocation}[];
  fixes?: {title: string; edits: {location: DiagnosticLocation; replacement: string}[]}[];
  attachments?: {kind: 'blob' | 'artifact'; id: string; label?: string}[];
}
export declare function validateDiagnostics(values: Diagnostic[]): void;
export declare function diagnosticSummary(values: Diagnostic[]): string;
export type MediaPart = {type: 'image'; artifact_id: string; mime_type: string; alt?: string} |
  {type: 'audio'; artifact_id: string; mime_type: string; transcript?: string};
declare const resourceBrand: unique symbol;
declare const blobBrand: unique symbol;
export interface ResourceRef<T extends object = object> {readonly $resource: string; readonly type: string; readonly [resourceBrand]?: T;}
export interface ResourceSchema<T extends object> extends Schema {readonly [resourceBrand]: T; readonly type: 'object';}
export interface ResourceType<T extends object> {readonly name: string; readonly schema: ResourceSchema<T>;}
export declare function resourceType<T extends object>(name: string, dispose?: (value: T) => void | Promise<void>): ResourceType<T>;
export interface BlobRef {readonly $blob: string; readonly bytes: number; readonly digest: {readonly algorithm: 'sha256'; readonly value: string}; readonly media_type: string;}
export interface BlobSchema extends Schema {readonly [blobBrand]: true; readonly type: 'object';}
export declare const blobSchema: BlobSchema;
export declare function validateBlob(value: unknown): BlobRef;
/** Transport-only metadata. Low-level grants require secure author-owned file I/O; not domain data. */
export interface BulkConfiguration {
  readonly profile: 'local-file.v1'; readonly transfer_directory: string;
  readonly limits: Readonly<{object_bytes: number; owner_bytes: number; write_tickets_per_generation: number; read_leases_per_generation: number; blobs_per_owner: number}>;
}

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
export type InferSchema<S> = S extends ResourceSchema<infer T> ? ResourceRef<T> : S extends BlobSchema ? BlobRef : S extends { readonly enum: readonly (infer V)[] } ? V :
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
  readonly bulk?: BulkConfiguration;
  supports(feature: string): boolean;
  /** Existing wire services, bounded and correlated to this live parent. Authority fields are SDK-owned. */
  request(method: HostMethod, params?: Readonly<Record<string, unknown>>): Promise<unknown>;
  exportResource<T extends object>(type: ResourceType<T>, value: T): Promise<ResourceRef<T>>;
  /** Native value may be used only within this admitted handler, never retained for background work. */
  resource<T extends object>(reference: ResourceRef<T>): T;
  releaseResource(reference: ResourceRef<object>): Promise<unknown>;
  /** Inline publication (at most 256 KiB); the host verifies signatures and owner authority. */
  publishArtifact(bytes: Uint8Array, mimeType: string): Promise<string>;
  throwIfCancelled(): void;
  /** Cooperative, abortable delay. Long handlers must yield between effects. */
  sleep(ms: number): Promise<void>;
  /** Ephemeral status; throws UnsupportedFeatureError if not offered by the host. */
  progress(message: string, counters?: {current?: number; total?: number; unit?: string}): Promise<void>;
}
export interface ToolResult<T = unknown> { text: string; isError?: boolean; structuredContent?: T; diagnostics?: Diagnostic[]; media?: MediaPart[]; }
export interface ToolDefinition<S extends Schema = Schema, O extends Schema = Schema> {
  name: string;
  description: string;
  parameters: S & {readonly type: 'object'};
  outputSchema?: O;
  receiver?: string;
}
export interface CommandDefinition { name: string; description: string; usage?: string; }
export declare class Extension {
  constructor(options?: {maxConcurrentRequests?: number; features?: ServiceFeature[]});
  tool<const S extends Schema, const O extends Schema = Schema>(definition: ToolDefinition<S, O>, handler:
    (arguments_: InferSchema<S>, context: RequestContext) => string | ToolResult<InferSchema<O>> | Promise<string | ToolResult<InferSchema<O>>>): this;
  /** Schema-checked structured value plus explicit model text, at most 64 KiB. */
  typedTool<const S extends Schema, const O extends Schema>(definition: ToolDefinition<S, O> & {outputSchema: O}, handler:
    (arguments_: InferSchema<S>, context: RequestContext) => InferSchema<O> | Promise<InferSchema<O>>,
    project: (value: InferSchema<O>) => string): this;
  command(definition: CommandDefinition, handler:
    (arguments_: string[], context: RequestContext) => string | Promise<string>): this;
  hook(name: HookName, handler: (payload: Readonly<Record<string, unknown>>, context: RequestContext) => HookResult | void | Promise<HookResult | void>): this;
  onShutdown(handler: (context: {readonly reason: 'shutdown' | 'transport_lost'}) => void | Promise<void>): this;
  contributions(): {tools: string[]; commands: string[]; hooks: string[]};
  /** Owns stdio and exits the process at its bounded terminal boundary. */
  run(): void;
}
