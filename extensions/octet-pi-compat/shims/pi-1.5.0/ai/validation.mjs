// Pi 1.5.0 tool-argument validation, adapted from
// packages/ai/src/utils/validation.ts (reference v1.5.0, 9c9ce8547).
//
// Pi 1.5 replaced TypeBox's validator with pi-schema:
//   - a tool call carrying `argumentsError` fails before any schema work, with
//     `Invalid arguments for tool "<name>": <argumentsError>`;
//   - arguments are cloned, coerced (`parse()`, coerce: true) and then checked,
//     so a valid call never surfaces an error for a value coerce handles;
//   - failures list pi-schema's messages (`- edits.0.oldText: missing`,
//     `- limit: expected number, got string`) with dotted JSON-pointer paths.
//
// This port keeps the same observable contract on the emulated path, where no
// installed Pi answers. Where a supported installed Pi is present the shim
// prefers Pi's own dist/utils/validation.js instead (see shims/ai.mjs and the
// manifest classification), so the schema engine itself is Pi's. The two
// documented differences from pi-schema are deliberate and invisible to valid
// arguments: TypeBox-only features (Refine, Codec, BigInt/Undefined/Void,
// Format.Set) do not apply to a plain JSON Schema tool, and a keyword this
// mapping does not name falls back to TypeBox's own message text.

import { Compile } from "typebox/compile";
import { Value } from "typebox/value";
const validatorCache = new WeakMap();
const TYPEBOX_KIND = Symbol.for("TypeBox.Kind");
function getSchemaTypes(schema) {
    if (typeof schema.type === "string") {
        return [schema.type];
    }
    if (Array.isArray(schema.type)) {
        return schema.type.filter((type) => typeof type === "string");
    }
    return [];
}
function matchesJsonType(value, type) {
    switch (type) {
        case "number":
            return typeof value === "number";
        case "integer":
            return typeof value === "number" && Number.isInteger(value);
        case "boolean":
            return typeof value === "boolean";
        case "string":
            return typeof value === "string";
        case "null":
            return value === null;
        case "array":
            return Array.isArray(value);
        case "object":
            return typeof value === "object" && value !== null && !Array.isArray(value);
        default:
            return false;
    }
}
function getSubSchemaValidator(schema) {
    try {
        return getValidator(schema);
    }
    catch {
        return undefined;
    }
}
function coercePrimitiveByType(value, type) {
    switch (type) {
        case "number": {
            if (value === null) {
                return 0;
            }
            if (typeof value === "string" && value.trim() !== "") {
                const parsed = Number(value);
                if (Number.isFinite(parsed)) {
                    return parsed;
                }
            }
            if (typeof value === "boolean") {
                return value ? 1 : 0;
            }
            return value;
        }
        case "integer": {
            if (value === null) {
                return 0;
            }
            if (typeof value === "string" && value.trim() !== "") {
                const parsed = Number(value);
                if (Number.isInteger(parsed)) {
                    return parsed;
                }
            }
            if (typeof value === "boolean") {
                return value ? 1 : 0;
            }
            return value;
        }
        case "boolean": {
            if (value === null) {
                return false;
            }
            if (typeof value === "string") {
                if (value === "true") {
                    return true;
                }
                if (value === "false") {
                    return false;
                }
            }
            if (typeof value === "number") {
                if (value === 1) {
                    return true;
                }
                if (value === 0) {
                    return false;
                }
            }
            return value;
        }
        case "string": {
            if (value === null) {
                return "";
            }
            if (typeof value === "number" || typeof value === "boolean") {
                return String(value);
            }
            return value;
        }
        case "null": {
            if (value === "" || value === 0 || value === false) {
                return null;
            }
            return value;
        }
        default:
            return value;
    }
}
function applySchemaObjectCoercion(value, schema) {
    const properties = schema.properties;
    const definedKeys = new Set(properties ? Object.keys(properties) : []);
    if (properties) {
        for (const [key, propertySchema] of Object.entries(properties)) {
            if (!(key in value)) {
                continue;
            }
            value[key] = coerceWithJsonSchema(value[key], propertySchema);
        }
    }
    if (schema.additionalProperties && typeof schema.additionalProperties === "object") {
        for (const [key, propertyValue] of Object.entries(value)) {
            if (definedKeys.has(key)) {
                continue;
            }
            value[key] = coerceWithJsonSchema(propertyValue, schema.additionalProperties);
        }
    }
}
function applySchemaArrayCoercion(value, schema) {
    if (Array.isArray(schema.items)) {
        for (let index = 0; index < value.length; index++) {
            const itemSchema = schema.items[index];
            if (!itemSchema) {
                continue;
            }
            value[index] = coerceWithJsonSchema(value[index], itemSchema);
        }
        return;
    }
    if (schema.items && typeof schema.items === "object") {
        for (let index = 0; index < value.length; index++) {
            value[index] = coerceWithJsonSchema(value[index], schema.items);
        }
    }
}
function coerceWithUnionSchema(value, schemas) {
    for (const schema of schemas) {
        const validator = getSubSchemaValidator(schema);
        if (validator?.Check(value)) {
            return value;
        }
    }
    for (const schema of schemas) {
        const candidate = structuredClone(value);
        const coerced = coerceWithJsonSchema(candidate, schema);
        const validator = getSubSchemaValidator(schema);
        if (validator?.Check(coerced)) {
            return coerced;
        }
    }
    return value;
}
function coerceWithJsonSchema(value, schema) {
    let nextValue = value;
    if (Array.isArray(schema.allOf)) {
        for (const nested of schema.allOf) {
            nextValue = coerceWithJsonSchema(nextValue, nested);
        }
    }
    if (Array.isArray(schema.anyOf)) {
        nextValue = coerceWithUnionSchema(nextValue, schema.anyOf);
    }
    if (Array.isArray(schema.oneOf)) {
        nextValue = coerceWithUnionSchema(nextValue, schema.oneOf);
    }
    const schemaTypes = getSchemaTypes(schema);
    const matchesUnionMember = schemaTypes.length > 1 && schemaTypes.some((schemaType) => matchesJsonType(nextValue, schemaType));
    if (schemaTypes.length > 0 && !matchesUnionMember) {
        for (const schemaType of schemaTypes) {
            const candidate = coercePrimitiveByType(nextValue, schemaType);
            if (candidate !== nextValue) {
                nextValue = candidate;
                break;
            }
        }
    }
    if (schemaTypes.includes("object") &&
        typeof nextValue === "object" &&
        nextValue !== null &&
        !Array.isArray(nextValue)) {
        applySchemaObjectCoercion(nextValue, schema);
    }
    if (schemaTypes.includes("array") && Array.isArray(nextValue)) {
        applySchemaArrayCoercion(nextValue, schema);
    }
    return nextValue;
}
function normalizeOptionalNulls(value, schema) {
    if (Array.isArray(value)) {
        if (Array.isArray(schema.items)) {
            for (let index = 0; index < value.length; index++) {
                const itemSchema = schema.items[index];
                if (itemSchema)
                    normalizeOptionalNulls(value[index], itemSchema);
            }
        }
        else if (schema.items) {
            for (const item of value)
                normalizeOptionalNulls(item, schema.items);
        }
        return;
    }
    if (typeof value !== "object" || value === null || !schema.properties)
        return;
    const object = value;
    const required = new Set(schema.required ?? []);
    for (const [key, propertySchema] of Object.entries(schema.properties)) {
        if (!(key in object))
            continue;
        if (object[key] === null &&
            !required.has(key) &&
            typeof propertySchema.$ref !== "string" &&
            getSubSchemaValidator(propertySchema)?.Check(null) === false) {
            delete object[key];
        }
        else {
            normalizeOptionalNulls(object[key], propertySchema);
        }
    }
}
function getValidator(schema) {
    const key = schema;
    const cached = validatorCache.get(key);
    if (cached) {
        return cached;
    }
    const validator = Compile(schema);
    validatorCache.set(key, validator);
    return validator;
}
const LISTED_VALUES = 4;
const listValues = values => {
    if (values.length <= LISTED_VALUES) {
        return JSON.stringify(values);
    }
    return `${JSON.stringify(values.slice(0, LISTED_VALUES)).slice(0, -1)}, ... and ${values.length - LISTED_VALUES} more]`;
};
// pi-schema explain.ts typeName().
function typeName(value) {
    if (value === null) {
        return "null";
    }
    if (Array.isArray(value)) {
        return "array";
    }
    if (value instanceof Uint8Array) {
        return "bytes";
    }
    if (typeof value === "number" && Number.isInteger(value)) {
        return "integer";
    }
    return typeof value;
}
function valueAtPointer(root, pointer) {
    if (!pointer) {
        return root;
    }
    let value = root;
    for (const raw of pointer.split("/").slice(1)) {
        const key = raw.replace(/~1/g, "/").replace(/~0/g, "~");
        if (value === null || typeof value !== "object") {
            return undefined;
        }
        value = value[key];
    }
    return value;
}
// A pi-schema error path (a JSON Pointer) as the dotted path the model sees:
// `/edits/0/oldText` -> `edits.0.oldText`.
function formatValidationPath(pointer) {
    if (!pointer) {
        return "root";
    }
    return pointer
        .slice(1)
        .split("/")
        .map((segment) => segment.replace(/~1/g, "/").replace(/~0/g, "~"))
        .join(".");
}
// pi-schema explain.ts messages, so a failure reads the way Pi 1.5 reports it.
function piSchemaErrors(error, args) {
    switch (error.keyword) {
        case "required": {
            const properties = error.params.requiredProperties ?? [];
            return { path: `${error.instancePath}/${properties[0] ?? ""}`, message: "missing" };
        }
        case "type": {
            const expected = error.params.type;
            const types = Array.isArray(expected) ? expected.join(" or ") : String(expected);
            return { path: error.instancePath, message: `expected ${types}, got ${typeName(valueAtPointer(args, error.instancePath))}` };
        }
        case "enum":
            return { path: error.instancePath, message: `expected one of ${listValues(error.params.allowedValues ?? [])}` };
        case "const":
            return { path: error.instancePath, message: `expected ${JSON.stringify(error.params.allowedValue)}` };
        case "additionalProperties":
            return (error.params.additionalProperties ?? []).map((key) => ({ path: `${error.instancePath}/${key}`, message: "unknown field" }));
        case "minLength":
            return { path: error.instancePath, message: `expected at least ${error.params.limit} characters` };
        case "maxLength":
            return { path: error.instancePath, message: `expected at most ${error.params.limit} characters` };
        case "pattern":
            return { path: error.instancePath, message: `expected to match ${error.params.pattern}` };
        case "format":
            return { path: error.instancePath, message: `expected format ${error.params.format}` };
        case "minimum":
            return { path: error.instancePath, message: `expected >= ${error.params.limit}` };
        case "maximum":
            return { path: error.instancePath, message: `expected <= ${error.params.limit}` };
        case "multipleOf":
            return { path: error.instancePath, message: `expected a multiple of ${error.params.multipleOf}` };
        case "minItems":
            return { path: error.instancePath, message: `expected at least ${error.params.limit} items` };
        case "maxItems":
            return { path: error.instancePath, message: `expected at most ${error.params.limit} items` };
        case "minProperties":
            return { path: error.instancePath, message: `expected at least ${error.params.limit} properties` };
        case "maxProperties":
            return { path: error.instancePath, message: `expected at most ${error.params.limit} properties` };
        default:
            return { path: error.instancePath, message: error.message };
    }
}

export function validateToolCall(tools, toolCall) {
    const tool = tools.find((t) => t.name === toolCall.name);
    if (!tool) {
        throw new Error(`Tool "${toolCall.name}" not found`);
    }
    return validateToolArguments(tool, toolCall);
}

export function validateToolArguments(tool, toolCall) {
    if (toolCall.argumentsError) {
        throw new Error(`Invalid arguments for tool "${toolCall.name}": ${toolCall.argumentsError}`);
    }
    const args = structuredClone(toolCall.arguments);
    normalizeOptionalNulls(args, tool.parameters);
    Value.Convert(tool.parameters, args);
    const validator = getValidator(tool.parameters);
    if (!Object.getOwnPropertySymbols(tool.parameters).includes(TYPEBOX_KIND)) {
        const coerced = coerceWithJsonSchema(args, tool.parameters);
        if (coerced !== args) {
            if (typeof args === "object" && args !== null && typeof coerced === "object" && coerced !== null) {
                for (const key of Object.keys(args)) {
                    delete args[key];
                }
                Object.assign(args, coerced);
            }
            else {
                return validator.Check(coerced) ? coerced : args;
            }
        }
    }
    if (validator.Check(args)) {
        return args;
    }
    const errors = validator
        .Errors(args)
        // pi-schema reports a rejected additional property once ("unknown
        // field"); TypeBox also reports the false subschema it came from.
        .filter((error) => !(error.keyword === "boolean" && error.schemaPath.endsWith("/additionalProperties")))
        .flatMap((error) => [].concat(piSchemaErrors(error, args)).map(({ path, message }) => `  - ${formatValidationPath(path)}: ${message}`))
        .join("\n") || "Unknown validation error";
    const errorMessage = `Validation failed for tool "${toolCall.name}":\n${errors}\n\nReceived arguments:\n${JSON.stringify(toolCall.arguments, null, 2)}`;
    throw new Error(errorMessage);
}
