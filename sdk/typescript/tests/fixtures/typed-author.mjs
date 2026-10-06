import {appendFileSync} from 'node:fs';
import { Extension, ToolError } from '../../process/index.mjs';

// Optional native-host evidence, never a protocol peer or part of tool output.
// The host fixture owns this private file; normal Node tests do not set it.
const record = (event, details = {}) => {
  if (process.env.OCTET_TS_TYPED_LOG) appendFileSync(process.env.OCTET_TS_TYPED_LOG,
    JSON.stringify({event, pid: process.pid, ...details}) + '\n');
};
const extension = new Extension();
extension.onShutdown(({reason}) => record('shutdown', {reason}));
const parameters = {type: 'object', properties: {
  text: {type: 'string'}, mode: {type: 'string'},
}, additionalProperties: false};
const outputSchema = {type: 'object', properties: {
  characters: {type: 'integer', minimum: 0}, note: {type: 'null'},
}, required: ['characters', 'note'], additionalProperties: false};
extension.typedTool({name: 'typed_stats', description: 'Count Unicode characters', parameters, outputSchema},
  async ({text = '', mode}, context) => {
    record('call', {tool: 'typed_stats', mode: mode ?? 'valid', home: process.env.HOME});
    if (mode === 'error') throw new ToolError('Expected domain failure');
    if (mode === 'invalid') return {characters: 'wrong', note: null};
    if (mode === 'nonfinite') return {characters: Infinity, note: null};
    if (mode === 'extra') return {characters: 1, note: null, extra: true};
    if (mode === 'cancel') {
      if (context.supportsProgress) await context.progress('entered');
      record('entered');
      try { await context.sleep(30000); }
      catch (error) { record('cancelled', {aborted: context.signal.aborted}); throw error; }
    }
    record('returned');
    return {characters: [...text].length, note: null};
  }, value => `${value.characters} Unicode characters`);
extension.tool({name: 'typed_null', description: 'Return an explicit typed null',
  parameters: {type: 'object', properties: {diagnostics: {type: 'boolean'}, invalidDiagnostic: {type: 'boolean'}}, additionalProperties: false}, outputSchema: {type: 'null'}},
  ({diagnostics, invalidDiagnostic}) => {
    record('call', {tool: 'typed_null', home: process.env.HOME});
    return {text: 'No value', structuredContent: null,
      ...(diagnostics || invalidDiagnostic ? {diagnostics: [{severity: 'warning', code: 'fixture.note', message: invalidDiagnostic ? '\u001b[31m' : 'No samples\navailable'}]} : {})};
  });
export default extension;
