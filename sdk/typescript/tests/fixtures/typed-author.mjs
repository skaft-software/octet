import { Extension, ToolError } from '../../process/index.mjs';

const extension = new Extension();
const parameters = {type: 'object', properties: {
  text: {type: 'string'}, mode: {type: 'string'},
}, additionalProperties: false};
const outputSchema = {type: 'object', properties: {
  characters: {type: 'integer', minimum: 0}, note: {type: 'null'},
}, required: ['characters', 'note'], additionalProperties: false};
extension.typedTool({name: 'typed_stats', description: 'Count Unicode characters', parameters, outputSchema},
  async ({text = '', mode}, context) => {
    if (mode === 'error') throw new ToolError('Expected domain failure');
    if (mode === 'invalid') return {characters: 'wrong', note: null};
    if (mode === 'nonfinite') return {characters: Infinity, note: null};
    if (mode === 'extra') return {characters: 1, note: null, extra: true};
    if (mode === 'cancel') { await context.progress('entered'); await context.sleep(30000); }
    return {characters: [...text].length, note: null};
  }, value => `${value.characters} Unicode characters`);
extension.tool({name: 'typed_null', description: 'Return an explicit typed null',
  parameters: {type: 'object', additionalProperties: false}, outputSchema: {type: 'null'}},
  () => ({text: 'No value', structuredContent: null}));
export default extension;
