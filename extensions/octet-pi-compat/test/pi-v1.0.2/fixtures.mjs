import { RemoteTUI } from '../../lib/remote-ui.mjs';

// These Editor unit suites never start a terminal, paint a viewport or deliver
// terminal input. Keep their geometry while exercising the adapter's actual
// headless TUI facade; importing Pi's terminal renderer is neither needed nor
// permitted. A native-backed Editor will also need its real sync host binding
// here: this geometry-only fixture must never implement editing in JavaScript.
export class VirtualTerminal {
  constructor(columns = 80, rows = 24) {
    this.columns = columns;
    this.rows = rows;
  }
}

export class TuiMainScreen {
  constructor(terminal) {
    return new RemoteTUI({
      columns: terminal.columns,
      rows: terminal.rows,
      requestRender() {},
    }).facade;
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
