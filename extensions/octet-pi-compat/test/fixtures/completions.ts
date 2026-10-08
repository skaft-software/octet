export default pi => {
  const calls = [], pending = new Map(), entered = new Map();
  const choice = prefix => [{ value: `${prefix}✓`, label: '選択', description: 'exact raw prefix' }];
  pi.registerCommand('complete', {
    description: 'Argument completion fixture',
    getArgumentCompletions(prefix) {
      calls.push(prefix); entered.get(prefix)?.(); entered.delete(prefix);
      if (prefix.startsWith('slow:')) return new Promise(resolve => pending.set(prefix, resolve));
      if (prefix === 'null') return null;
      if (prefix === 'empty') return [];
      if (prefix === 'sparse') return Array(1);
      if (prefix === 'non-array') return 'not an array';
      if (prefix === 'throw') throw new Error('completion callback failed');
      if (prefix === 'reject') return Promise.reject(new Error('async completion failed'));
      if (prefix === 'maximum') return Array.from({ length: 32 }, () => ({ value: '😀'.repeat(256), label: 'é'.repeat(512), description: 'x'.repeat(1024) }));
      if (prefix.length >= 1024) return [{ value: 'short value', label: 'short label' }];
      if (prefix === 'many') return Array.from({ length: 33 }, () => ({ value: 'v', label: 'v' }));
      if (prefix === 'long') return [{ value: '😀'.repeat(257), label: 'too many bytes' }];
      if (prefix === 'control') return [{ value: 'bad\x1b[31m', label: 'unsafe' }];
      if (prefix === 'control-label') return [{ value: 'v', label: '\u0085' }];
      if (prefix === 'control-description') return [{ value: 'v', label: 'v', description: '\n' }];
      if (prefix === 'bad-item') return [{ value: 'v', label: 7 }];
      if (prefix === 'unknown-field') return [{ value: 'v', label: 'v', effect: 'not representable' }];
      if (prefix === '"fo') return [{ value: '"folder"', label: 'folder' }];
      if (prefix === '"dir') return [{ value: '"folder/"', label: 'folder/' }];
      if (prefix === 'async') return Promise.resolve(choice(prefix));
      return choice(prefix);
    },
    handler: async (args, ctx) => { ctx.ui.notify(args); },
  });
  pi.registerCommand('plain', { handler: async () => {} });
  pi.registerTool({ name: 'completion_state', description: 'Read or release deterministic completion fixture state',
    parameters: { type: 'object', properties: { release: { type: 'string' }, wait_for: { type: 'string' } } },
    async execute(_id, args) {
      if (args.wait_for && !calls.includes(args.wait_for)) await new Promise(resolve => entered.set(args.wait_for, resolve));
      if (args.release) { pending.get(args.release)?.(choice(args.release)); pending.delete(args.release); }
      return { content: [{ type: 'text', text: JSON.stringify({ calls, pending: [...pending.keys()] }) }] };
    },
  });
};
