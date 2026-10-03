// BM25/discovery follows Pi's public codemode conventions. Declarations and
// identifier normalization are the actual vendored MIT Pi implementation.
import { renderToolSample, toCodemodeIdentifier } from "./vendor/pi-codemode/dist/index.js";
import { isObject, has, exactKeys, MAX_CALLS, MAX_HOST_FILE_BYTES, validateJson } from "./common.mjs";

const TYPES = new Set(["object", "array", "string", "number", "integer", "boolean", "null"]);
function schema(value, depth = 0) {
  if (typeof value === "boolean") return;
  if (!isObject(value) || depth > 32) throw new Error("Invalid composition tool JSON schema");
  if (has(value, "type") && !(typeof value.type === "string" ? TYPES.has(value.type) :
      Array.isArray(value.type) && value.type.length > 0 && new Set(value.type).size === value.type.length && value.type.every((type) => TYPES.has(type)))) {
    throw new Error("Invalid composition schema type");
  }
  for (const name of ["properties", "patternProperties", "$defs", "definitions", "dependentSchemas"]) {
    if (!has(value, name)) continue;
    if (!isObject(value[name])) throw new Error(`Invalid composition schema ${name}`);
    for (const child of Object.values(value[name])) schema(child, depth + 1);
  }
  for (const name of ["additionalProperties", "unevaluatedProperties", "not", "if", "then", "else", "contains", "propertyNames"]) {
    if (has(value, name)) schema(value[name], depth + 1);
  }
  if (has(value, "items")) {
    if (Array.isArray(value.items)) for (const child of value.items) schema(child, depth + 1);
    else schema(value.items, depth + 1);
  }
  for (const name of ["allOf", "anyOf", "oneOf", "prefixItems"]) {
    if (!has(value, name)) continue;
    if (!Array.isArray(value[name]) || value[name].length === 0) throw new Error(`Invalid composition schema ${name}`);
    for (const child of value[name]) schema(child, depth + 1);
  }
  if (has(value, "required") && (!Array.isArray(value.required) || !value.required.every((key) => typeof key === "string") || new Set(value.required).size !== value.required.length)) {
    throw new Error("Invalid composition schema required");
  }
  for (const name of ["minLength", "maxLength", "minItems", "maxItems", "minProperties", "maxProperties"]) {
    if (has(value, name) && (!Number.isSafeInteger(value[name]) || value[name] < 0)) throw new Error(`Invalid composition schema ${name}`);
  }
  for (const name of ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "multipleOf"]) {
    if (has(value, name) && (typeof value[name] !== "number" || !Number.isFinite(value[name]))) throw new Error(`Invalid composition schema ${name}`);
  }
  if (has(value, "enum") && (!Array.isArray(value.enum) || value.enum.length === 0)) throw new Error("Invalid composition schema enum");
  for (const name of ["description", "title", "$ref", "pattern", "format"]) {
    if (has(value, name) && typeof value[name] !== "string") throw new Error(`Invalid composition schema ${name}`);
  }
}

export function validateContext(context) {
  if (!exactKeys(context, ["tools", "store", "limits"]) || !Array.isArray(context.tools) || context.tools.length > 4096 ||
      !isObject(context.store) || !exactKeys(context.limits, ["timeout_ms", "max_calls"]) ||
      !Number.isSafeInteger(context.limits.timeout_ms) || context.limits.timeout_ms <= 0 || context.limits.timeout_ms > 30_000 ||
      !Number.isSafeInteger(context.limits.max_calls) || context.limits.max_calls <= 0 || context.limits.max_calls > MAX_CALLS) {
    throw new Error("Invalid composition/context response or limits");
  }
  validateJson(context);
  if (Buffer.byteLength(JSON.stringify(context)) > MAX_HOST_FILE_BYTES) throw new Error("Composition context exceeds 8 MiB");
  const names = new Set();
  for (const tool of context.tools) {
    if (!exactKeys(tool, ["name", "description", "parameters", "output_schema"]) || typeof tool.name !== "string" ||
        !/^[A-Za-z0-9_.:$-]{1,128}$/.test(tool.name) || typeof tool.description !== "string" ||
        !isObject(tool.parameters) || names.has(tool.name) || tool.name === "codemode") {
      throw new Error("Invalid/duplicate/recursive composition tool definition");
    }
    names.add(tool.name);
    schema(tool.parameters);
    if (has(tool.parameters, "type") && tool.parameters.type !== "object") throw new Error("Composition tool parameters must be an object schema");
    if (has(tool, "output_schema")) schema(tool.output_schema);
  }
  let total = 0;
  for (const value of Object.values(context.store)) {
    const chars = JSON.stringify(value).length;
    if (chars > 256 * 1024) throw new Error("Composition store value exceeds Pi's 256 Ki-character bound");
    total += chars;
  }
  if (total > 1024 * 1024) throw new Error("Composition store exceeds Pi's 1 Mi-character bound");
  return context;
}

export function toolNamespace(name) {
  const match = /^mcp__([^]+?)__/.exec(name);
  return match ? `mcp__${match[1]}` : undefined;
}
function namespaceMatches(namespace, name) {
  const id = toCodemodeIdentifier(namespace);
  const query = toCodemodeIdentifier(name);
  return namespace === name || id === query || namespace.slice(5) === name || id.slice(5) === query;
}
const STOP_WORDS = new Set("a an and are as at be by for from in is it of on or that the this to with".split(" "));
function terms(text) {
  return text.replace(/([a-z0-9])([A-Z])/g, "$1 $2").replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .toLowerCase().split(/[^a-z0-9]+/).filter((word) => word && !STOP_WORDS.has(word)).map((word) => {
      if (word.length > 4 && word.endsWith("ies")) return `${word.slice(0, -3)}y`;
      if (word.length > 4 && /(ches|shes|sses|xes|zes)$/.test(word)) return word.slice(0, -2);
      return word.length > 3 && word.endsWith("s") && !word.endsWith("ss") ? word.slice(0, -1) : word;
    });
}
function schemaWords(value, words) {
  if (!isObject(value)) return;
  if (typeof value.description === "string") words.push(value.description);
  if (isObject(value.properties)) for (const [key, child] of Object.entries(value.properties)) { words.push(key); schemaWords(child, words); }
  schemaWords(value.items, words);
  for (const key of ["allOf", "anyOf", "oneOf"]) if (Array.isArray(value[key])) for (const child of value[key]) schemaWords(child, words);
}
export function rankTools(query, tools, limit) {
  const queryTerms = [...new Set(terms(query))];
  if (!queryTerms.length || !tools.length) return [];
  const counts = tools.map((tool) => {
    const words = [tool.name, tool.name.replaceAll("_", " "), tool.description, toolNamespace(tool.name) ?? ""];
    schemaWords(tool.parameters, words);
    const counts = new Map();
    for (const term of terms(words.join(" "))) counts.set(term, (counts.get(term) ?? 0) + 1);
    return counts;
  });
  const lengths = counts.map((count) => [...count.values()].reduce((sum, n) => sum + n, 0));
  const average = lengths.reduce((sum, n) => sum + n, 0) / tools.length || 1;
  const scores = tools.map((tool, index) => {
    let score = 0;
    for (const term of queryTerms) {
      const count = counts[index].get(term) ?? 0;
      if (!count) continue;
      const frequency = counts.filter((counts) => counts.has(term)).length;
      const idf = Math.log(1 + (tools.length - frequency + 0.5) / (frequency + 0.5));
      score += idf * (count * 2.2) / (count + 1.2 * (0.25 + 0.75 * lengths[index] / average));
    }
    return { tool, score };
  });
  return scores.filter((row) => row.score > 0).sort((a, b) => b.score - a.score).slice(0, limit).map((row) => row.tool);
}

export function discovery(tools) {
  const samples = new Map(tools.map((tool) => [tool.name, renderToolSample({ name: tool.name, description: tool.description,
    inputSchema: tool.parameters, outputSchema: has(tool, "output_schema") ? tool.output_schema : { type: "string" } })]));
  const entry = (tool) => ({ name: toCodemodeIdentifier(tool.name), description: samples.get(tool.name) });
  const globals = [
    { name: "searchTools", spread: true, execute: ([query, options]) => {
      if (typeof query !== "string" || query.length > 4096) throw new Error("searchTools() expects a query string of at most 4096 characters");
      if (options !== undefined && !exactKeys(options, ["limit", "namespace"])) throw new Error("searchTools() options must contain only limit and namespace");
      const limit = options?.limit ?? 8;
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 256) throw new Error("searchTools() limit must be an integer from 1 to 256");
      const namespace = options?.namespace;
      if (namespace !== undefined && namespace !== null && typeof namespace !== "string") throw new Error("searchTools() namespace must be a string");
      const selected = namespace ? tools.filter((tool) => toolNamespace(tool.name) && namespaceMatches(toolNamespace(tool.name), namespace)) : tools;
      return rankTools(query, selected, limit).map(entry);
    } },
    { name: "describeTool", spread: true, execute: ([name]) => {
      if (typeof name !== "string") throw new Error("describeTool() expects a tool name");
      const tool = tools.find((tool) => tool.name === name || toCodemodeIdentifier(tool.name) === name);
      return tool ? samples.get(tool.name) : undefined;
    } },
    { name: "describeNamespace", spread: true, execute: ([name]) => {
      if (typeof name !== "string") throw new Error("describeNamespace() expects a namespace name");
      const matches = tools.filter((tool) => toolNamespace(tool.name) && namespaceMatches(toolNamespace(tool.name), name));
      return matches.length ? { name: toolNamespace(matches[0].name), tools: matches.map((tool) => toCodemodeIdentifier(tool.name)) } : undefined;
    } },
  ];
  return { globals, samples };
}
