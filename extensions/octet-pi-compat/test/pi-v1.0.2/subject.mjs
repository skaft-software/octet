// Exercise the export used by real extension factories, not an independently
// constructed reference Editor. Phase 2 replaces this export in one place.
export { Editor } from '../../shims/tui.mjs';

// Upstream also tests this pure, stateless wrapping utility (not exported by
// pi-tui's package root). These cases qualify the retained Pi library helper,
// not native Editor state or input ownership.
export { wordWrapLine } from '../../node_modules/@earendil-works/pi-tui/dist/components/editor.js';
