// Hermetic stand-in, not vendored Pi or a general JSON Schema validator.
// Production imports validateToolArguments from the selected runtime's public
// pi-ai export. These fixtures use only object schemas with string properties.
export function validateToolArguments(tool, call) {
  const args = structuredClone(call.arguments);
  const schema = tool.parameters;
  if (schema.type !== "object" || !args || typeof args !== "object" || Array.isArray(args)) {
    throw new Error("fixture expects object arguments");
  }
  for (const key of schema.required ?? []) {
    if (!(key in args)) throw new Error("required fixture argument missing");
  }
  for (const [key, value] of Object.entries(args)) {
    const property = schema.properties?.[key];
    if (!property) {
      if (schema.additionalProperties === false) throw new Error("additional fixture argument");
      continue;
    }
    if (property.type === "string" && ["number", "boolean"].includes(typeof value)) args[key] = String(value);
    if (property.type === "string" && typeof args[key] !== "string") throw new Error("invalid fixture argument");
  }
  return args;
}
