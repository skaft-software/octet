import { RemoteTUI } from '../../lib/remote-ui.mjs';
import { after, afterEach } from 'node:test';
import { nativeEditorHost } from '../native-editor-host.mjs';

const peer = nativeEditorHost({after});
const surfaces = [];
let sequence = 0;
afterEach(() => {
  for (const surface of surfaces.splice(0)) {
    surface.closed = true;
    peer.call('ui/close', {surface_id: surface.id});
  }
});

// These Editor unit suites never start a terminal, paint a viewport or deliver
// terminal input. Preserve geometry, binding the actual RemoteTUI and facade
// to the production Rust service. No editing operation is implemented here.
export class VirtualTerminal {
  constructor(columns = 80, rows = 24) {
    this.columns = columns;
    this.rows = rows;
  }
}

export class TuiMainScreen {
  constructor(terminal) {
    const surface = {id: `fixture-${++sequence}`, placement: 'fullscreen', opened: true, closed: false,
      columns: terminal.columns, rows: terminal.rows, requestRender() {},
      store: {id: 1, live: true, state: {owner: {session_id: 'fixture', extension_instance_id: 'fixture', process_generation: 1}}, controller: new AbortController()},
      runtime: {require() {}, assertOwner() {}, transport: {requestSync: peer.call}},
    };
    peer.call('ui/open', {surface_id: surface.id});
    surfaces.push(surface);
    return new RemoteTUI(surface).facade;
  }
}

// Upstream test-themes.ts uses Chalk level 3 with these exact SGR pairs.
// The tested strings do not require Chalk's terminal detection or nested styles.
const style = (open, close) => text => text === '' ? '' : `\x1b[${open}m${text}\x1b[${close}m`;
const dim = style(2, 22);
export const defaultEditorTheme = {
  borderColor: dim,
  selectList: {
    selectedPrefix: style(34, 39),
    selectedText: style(1, 22),
    description: dim,
    scrollInfo: dim,
    noMatch: dim,
  },
};
