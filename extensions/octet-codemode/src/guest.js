(contextJson, sourceJson) => {
const emit = globalThis.__emit;
delete globalThis.__emit;
const stringify = JSON.stringify, parse = JSON.parse;
const create = Object.create, assign = Object.assign;
const evaluate = (0, eval);
// Only byteLength is needed by the reused validation/discovery source.
const Buffer = {byteLength(value) {
 let bytes = 0; for (const c of value) {const n = c.codePointAt(0);
 bytes += n < 128 ? 1 : n < 2048 ? 2 : n < 65536 ? 3 : 4;} return bytes;
}};
@@MODULES@@
const context = validateContext(parse(contextJson));
const source = sourceJson;
const {globals, samples} = discovery(context.tools);
const helpers = new Map(globals.map(g => [g.name, g.execute]));
const definitions = context.tools.map(t => ({name:t.name,
 jsName:toCodemodeIdentifier(t.name), description:samples.get(t.name)}));
const saved = create(null);
for (const [key, value] of Object.entries(context.store)) saved[key] = stringify(value);
let terminal = false;
function post(fields) {
 if (terminal) return;
 if (fields.type === 'done' || fields.type === 'crash') terminal = true;
 emit(stringify(assign(create(null), fields)));
}
let helperCount = 0;
let api;
const bridge = (kind, a, b, c) => {
 if (kind === 'global') {
   if (++helperCount > 1024) {post({type:'crash',message:'Script exceeded 1024 discovery calls'}); return;}
   try {
     const result = helpers.get(b)(c === undefined ? [] : parse(c));
     api.settle(a, true, result === undefined ? undefined : stringify(result));
   } catch(e) { api.settle(a, false, String(e.message ?? e)); }
 } else if (kind === 'call') {
   post({type:'call',id:a,target:'tool',name:b,args:c});
 } else if (kind === 'output') {
   const item = a === 'image' ? {type:'image',data:b,mimeType:c} : {type:'text',text:b};
   post({type:'output',item:assign(create(null),item)});
 } else if (kind === 'done') {
   post(a ? {type:'done',ok:true,value:b,writes:c} : {type:'done',ok:false,error:b});
 }
};
api = evaluate(PRELUDE_SOURCE)(bridge, stringify(definitions),
 stringify(globals.map(g => ({name:g.name,spread:g.spread}))), stringify(saved));
return (command) => {
 if (terminal) return;
 const message = parse(command);
 if (message.type === 'start') {
   try { api.run(evaluate('(async (tools, console) => {' + source + '\n})\n//# sourceURL=codemode.js')); }
   catch(e) { post({type:'done',ok:false,error:stringify({name:e.name,message:e.message,stack:e.stack})}); }
 } else if (message.type === 'settle') api.settle(message.id,message.ok,message.payload);
 else if (message.type === 'poll') api.stalled();
};
}
