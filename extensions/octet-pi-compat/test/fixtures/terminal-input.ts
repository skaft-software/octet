let seen = [];
let off;
export default pi => {
  pi.registerCommand('listen', { handler: (mode, ctx) => {
    seen = [];
    if (mode === 'chain') {
      ctx.ui.onTerminalInput(data => { seen.push(['first', data]); return data === 'x' ? {consume: true} : data === 'a' ? {data: 'b'} : undefined; });
      ctx.ui.onTerminalInput(data => { seen.push(['second', data]); return data === 'b' ? {data: 'Z'} : undefined; });
    } else if (mode === 'set') {
      const later = data => { seen.push(['later', data]); return {data: 'L'}; };
      ctx.ui.onTerminalInput(data => { seen.push(['first', data]); off(); ctx.ui.onTerminalInput(later); });
      const duplicate = data => { seen.push(['removed', data]); return {consume:true}; };
      off = ctx.ui.onTerminalInput(duplicate);
      ctx.ui.onTerminalInput(duplicate);
    } else if (mode === 'empty') {
      ctx.ui.onTerminalInput(() => ({data: ''}));
      ctx.ui.onTerminalInput(data => { seen.push(['empty', data]); });
    } else if (mode === 'promise') {
      ctx.ui.onTerminalInput(() => new Promise(() => {}));
    } else if (mode === 'oversize') {
      ctx.ui.onTerminalInput(() => ({data: 'é'.repeat(129)}));
    } else if (mode === 'remove') off();
    else off = ctx.ui.onTerminalInput(data => ({data: `[${data}]`}));
  }});
  pi.registerTool({name:'terminal_state', label:'state', description:'listener trace', parameters:{type:'object', properties:{}},
    execute: async () => ({content:[{type:'text', text:JSON.stringify(seen)}]})});
}
