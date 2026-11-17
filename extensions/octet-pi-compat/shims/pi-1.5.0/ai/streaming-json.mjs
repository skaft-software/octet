// Adapted from Pi packages/ai/src/utils/streaming-json.ts:1-445,502-559.
// Source SHA256: eb237f041513bae5780b7a6f526c91028bb9eb1bf78b7dd5a5f7dc1b8f712210
// Copyright (c) 2025 Mario Zechner. MIT; see ../../../LICENSE.pi.
// Type annotations and the unused ToolCallArgumentsStream wrapper are omitted.
// append() reads each character once; snapshot() copies only the open path and
// spends credit earned by append() to keep total copy work linear. Large open
// containers may lag until another snapshot is affordable. Returned snapshots
// never change: completed containers are shared, open containers are copied.
const MAX_REPORTED_DEPTH = 32;
const ROOT_SLOT = {
    kind: "root"
};
const CREDIT_PER_APPEND = 256;
const CREDIT_PER_CHARACTER = 8;
const MAX_PENDING_SCALAR_LENGTH = 64;
const NUMBER = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/;
const NUMBER_WITHOUT_EXPONENT_DIGITS = /^(-?(?:0|[1-9]\d*)(?:\.\d+)?)[eE][+-]?$/;
const LITERALS = [
    [
        "true",
        true
    ],
    [
        "false",
        false
    ],
    [
        "null",
        null
    ]
];
const QUOTE = 0x22;
const BACKSLASH = 0x5c;
export class StreamingJsonParser {
    #root = undefined;
    #done = false;
    #stack = [];
    #string = undefined;
    #scalar = "";
    #scalarSlot = undefined;
    #pathCost = 0;
    #credit = 0;
    #dirty = false;
    #snapshot = undefined;
    #changes = [];
    append(text) {
        this.#credit += CREDIT_PER_APPEND + CREDIT_PER_CHARACTER * text.length;
        this.#changes = [];
        if (this.#string !== undefined) this.#string.change = undefined;
        let index = 0;
        while(index < text.length && !this.#done){
            const open = this.#string;
            if (open !== undefined) index = this.#readString(open, text, index);
            else if (this.#scalar !== "") index = this.#readScalar(text, index);
            else this.#structural(text[index++]);
        }
        return this.#changes;
    }
    snapshot() {
        if (!this.#dirty) return this.#snapshot;
        const pending = this.#pendingScalar();
        const cost = this.#pathCost + (pending === undefined ? 0 : 1);
        if (cost > this.#credit) return this.#snapshot;
        this.#credit -= cost;
        this.#dirty = false;
        this.#snapshot = this.#copyOpenPath(pending);
        return this.#snapshot;
    }
    #readString(open, text, start) {
        let runStart = start;
        for(let index = start; index < text.length; index++){
            const code = text.charCodeAt(index);
            if (open.escape !== "") {
                if (this.#continueEscape(open, text[index])) {
                    runStart = index + 1;
                    continue;
                }
                runStart = index;
            }
            if (code === QUOTE) {
                if (index > runStart) this.#appendString(open, text.slice(runStart, index));
                this.#endString(open);
                return index + 1;
            }
            if (code === BACKSLASH) {
                if (index > runStart) this.#appendString(open, text.slice(runStart, index));
                open.escape = "\\";
                runStart = index + 1;
            }
        }
        if (text.length > runStart) this.#appendString(open, text.slice(runStart));
        return text.length;
    }
    #readScalar(text, start) {
        let end = start;
        while(end < text.length && !isDelimiter(text[end]))end++;
        if (end > start) {
            this.#scalar += text.slice(start, end);
            if (this.#scalarSlot !== undefined) this.#dirty = true;
        }
        if (end < text.length) this.#endScalar();
        return end;
    }
    #structural(char) {
        switch(char){
            case " ":
            case "\t":
            case "\n":
            case "\r":
                return;
            case '"':
                this.#startString();
                return;
            case "{":
            case "[":
                this.#openContainer(char === "{" ? {} : []);
                return;
            case "}":
            case "]":
                this.#closeContainer();
                return;
            case ":":
                {
                    const frame = this.#stack.at(-1);
                    if (frame !== undefined && !Array.isArray(frame.container) && frame.phase === "colon") frame.phase = "value";
                    return;
                }
            case ",":
                {
                    const frame = this.#stack.at(-1);
                    if (frame !== undefined) frame.phase = Array.isArray(frame.container) ? "value" : "key";
                    return;
                }
            default:
                this.#scalar = char;
                this.#scalarSlot = this.#valueSlot();
                if (this.#scalarSlot !== undefined) this.#dirty = true;
        }
    }
    #valueSlot() {
        const frame = this.#stack.at(-1);
        if (frame === undefined) return this.#root === undefined ? ROOT_SLOT : undefined;
        if (Array.isArray(frame.container)) return {
            kind: "element",
            index: frame.container.length
        };
        if ((frame.phase === "value" || frame.phase === "colon") && frame.key !== undefined) {
            return {
                kind: "member",
                key: frame.key
            };
        }
        return undefined;
    }
    #startString() {
        const frame = this.#stack.at(-1);
        if (frame !== undefined && !Array.isArray(frame.container) && (frame.phase === "key" || frame.phase === "comma")) {
            this.#string = {
                target: "key",
                slot: undefined,
                text: "",
                escape: "",
                path: undefined,
                change: undefined
            };
            return;
        }
        const slot = this.#valueSlot();
        const path = this.#reportedPath(slot);
        this.#string = {
            target: "value",
            slot,
            text: "",
            escape: "",
            path,
            change: undefined
        };
        this.#place(slot, "");
        if (path !== undefined) this.#string.change = this.#report({
            kind: "set",
            path,
            value: ""
        });
    }
    #reportedPath(slot) {
        if (slot === undefined) return undefined;
        if (slot.kind === "root") return [];
        const frame = this.#stack.at(-1);
        if (!frame.reportsInside) return undefined;
        return [
            ...frame.path,
            slot.kind === "member" ? slot.key : slot.index
        ];
    }
    #report(change) {
        this.#changes.push(change);
        return change;
    }
    #continueEscape(open, char) {
        if (open.escape === "\\") {
            const decoded = SIMPLE_ESCAPES[char];
            if (decoded !== undefined) {
                open.escape = "";
                this.#appendString(open, decoded);
                return true;
            }
            if (char === "u") {
                open.escape = "\\u";
                return true;
            }
            open.escape = "";
            this.#appendString(open, "\\");
            return false;
        }
        if (isHexDigit(char)) {
            open.escape += char;
            if (open.escape.length < 6) return true;
            const decoded = String.fromCharCode(Number.parseInt(open.escape.slice(2), 16));
            open.escape = "";
            this.#appendString(open, decoded);
            return true;
        }
        const literal = open.escape;
        open.escape = "";
        this.#appendString(open, literal);
        return false;
    }
    #appendString(open, text) {
        open.text += text;
        if (open.target !== "value") return;
        this.#place(open.slot, open.text);
        if (open.path === undefined) return;
        const change = open.change;
        if (change !== undefined && this.#changes.at(-1) === change) {
            if (change.kind === "set") change.value = change.value + text;
            else change.text += text;
            return;
        }
        open.change = this.#report({
            kind: "append",
            path: open.path,
            text
        });
    }
    #endString(open) {
        this.#string = undefined;
        const frame = this.#stack.at(-1);
        if (open.target === "key") {
            if (frame === undefined) return;
            frame.key = open.text;
            frame.phase = "colon";
            return;
        }
        this.#valueEnded(frame);
    }
    #endScalar() {
        const token = this.#scalar;
        const slot = this.#scalarSlot;
        this.#scalar = "";
        this.#scalarSlot = undefined;
        const value = scalarValue(token);
        if (value !== undefined) {
            this.#place(slot, value);
            const path = this.#reportedPath(slot);
            if (path !== undefined) this.#report({
                kind: "set",
                path,
                value
            });
        }
        this.#valueEnded(this.#stack.at(-1));
    }
    #pendingScalar() {
        const slot = this.#scalarSlot;
        const token = this.#scalar;
        if (slot === undefined || token === "" || token.length > MAX_PENDING_SCALAR_LENGTH) return undefined;
        const value = pendingScalarValue(token);
        return value === undefined ? undefined : {
            slot,
            value
        };
    }
    #openContainer(container) {
        const slot = this.#valueSlot();
        const path = this.#reportedPath(slot);
        this.#place(slot, container);
        this.#stack.push({
            container,
            slot,
            path,
            reportsInside: path !== undefined && path.length < MAX_REPORTED_DEPTH,
            size: 0,
            key: undefined,
            phase: Array.isArray(container) ? "value" : "key"
        });
        this.#pathCost += 1;
        this.#dirty = true;
        if (path !== undefined) this.#report({
            kind: "set",
            path,
            value: Array.isArray(container) ? [] : {}
        });
    }
    #closeContainer() {
        const frame = this.#stack.pop();
        if (frame === undefined) return;
        this.#pathCost -= frame.size + 1;
        this.#dirty = true;
        if (frame.path !== undefined && !frame.reportsInside) {
            this.#report({
                kind: "set",
                path: frame.path,
                value: frame.container
            });
        }
        this.#valueEnded(this.#stack.at(-1));
    }
    #valueEnded(frame) {
        if (frame === undefined) {
            if (this.#root !== undefined) this.#done = true;
            return;
        }
        frame.phase = "comma";
    }
    #place(slot, value) {
        if (slot === undefined) return;
        this.#dirty = true;
        if (slot.kind === "root") {
            this.#root = value;
            return;
        }
        const frame = this.#stack.at(-1);
        if (slot.kind === "element") {
            const elements = frame.container;
            if (slot.index === elements.length) {
                frame.size++;
                this.#pathCost++;
            }
            elements[slot.index] = value;
            return;
        }
        const members = frame.container;
        if (!Object.hasOwn(members, slot.key)) {
            frame.size++;
            this.#pathCost++;
        }
        setMember(members, slot.key, value);
    }
    #copyOpenPath(pending) {
        if (this.#stack.length === 0) return pending?.slot.kind === "root" ? pending.value : this.#root;
        let child;
        let childSlot;
        for(let depth = this.#stack.length - 1; depth >= 0; depth--){
            const frame = this.#stack[depth];
            const copy = Array.isArray(frame.container) ? frame.container.slice() : {
                ...frame.container
            };
            if (depth === this.#stack.length - 1 && pending !== undefined) setSlot(copy, pending.slot, pending.value);
            if (child !== undefined && childSlot !== undefined) setSlot(copy, childSlot, child);
            child = copy;
            childSlot = frame.slot;
        }
        return childSlot?.kind === "root" ? child : this.#root;
    }
}
const SIMPLE_ESCAPES = {
    '"': '"',
    "\\": "\\",
    "/": "/",
    b: "\b",
    f: "\f",
    n: "\n",
    r: "\r",
    t: "\t"
};
function isDelimiter(char) {
    return char === " " || char === "\t" || char === "\n" || char === "\r" || char === "," || char === ":" || char === "{" || char === "}" || char === "[" || char === "]" || char === '"';
}
function isHexDigit(char) {
    return char >= "0" && char <= "9" || char >= "a" && char <= "f" || char >= "A" && char <= "F";
}
function scalarValue(token) {
    for (const [literal, value] of LITERALS)if (token === literal) return value;
    return NUMBER.test(token) ? Number(token) : undefined;
}
function pendingScalarValue(token) {
    for (const [literal, value] of LITERALS)if (literal.startsWith(token)) return value;
    if (NUMBER.test(token)) return Number(token);
    const mantissa = NUMBER_WITHOUT_EXPONENT_DIGITS.exec(token);
    return mantissa === null ? undefined : Number(mantissa[1]);
}
function setSlot(container, slot, value) {
    if (slot.kind === "element") container[slot.index] = value;
    else if (slot.kind === "member") setMember(container, slot.key, value);
}
function setMember(members, key, value) {
    if (key === "__proto__") {
        Object.defineProperty(members, key, {
            value,
            writable: true,
            enumerable: true,
            configurable: true
        });
    } else {
        members[key] = value;
    }
}
