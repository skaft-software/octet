import {Extension} from '@skaft-software/octet-extension-sdk';
import {readFileSync} from 'node:fs';

const extension = new Extension();
const pathArgument = {type: 'object', properties: {path: {type: 'string'}},
  required: ['path'], additionalProperties: false} as const;
const count = (file: string) => `${file}: ${readFileSync(file, 'utf8').split(/\s+/).filter(Boolean).length} words`;

extension.tool({name: 'word_count', description: 'Count the words in a file', parameters: pathArgument},
  ({path: file}, context) => {
    context.throwIfCancelled();
    return count(file);
  });

extension.command({name: 'wordcount', description: 'Count the words in a file', usage: '/wordcount PATH'},
  (arguments_) => arguments_.length ? count(arguments_[0]) : 'usage: /wordcount PATH');

export default extension;
