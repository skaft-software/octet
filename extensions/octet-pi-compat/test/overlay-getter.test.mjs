import test from 'node:test';
import assert from 'node:assert/strict';
import { RemoteTUI } from '../lib/remote-ui.mjs';

const component = () => ({ render: () => ['overlay'] });
const surface = () => ({ columns: 80, rows: 24, requestRender() {} });

test('overlay getter is supported on the real facade without weakening unknown operations', () => {
  const tui = new RemoteTUI(surface());
  const { getTopmostVisibleOverlay } = tui.facade;
  assert.equal(typeof getTopmostVisibleOverlay, 'function');
  assert.equal(getTopmostVisibleOverlay.call(tui.facade), undefined);
  tui.facade.addChild(component());
  assert.equal(tui.facade.getTopmostVisibleOverlay(), undefined);
  assert.throws(() => tui.facade.unknownOverlayOperation(), /unsupported_feature tui.unknownOverlayOperation/);
});

test('overlay getter returns the maintained capturing entry and tracks handle focus order', () => {
  const tui = new RemoteTUI(surface());
  const first = component(), second = component();
  const firstHandle = tui.facade.showOverlay(first);
  const firstEntry = tui.facade.getTopmostVisibleOverlay();
  assert.equal(firstEntry.component, first);
  assert.equal(firstEntry, tui.topOverlay());
  const secondHandle = tui.facade.showOverlay(second);
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, second);
  firstHandle.focus();
  assert.equal(tui.facade.getTopmostVisibleOverlay(), firstEntry);
  assert.equal(firstHandle.isFocused(), true);
  secondHandle.focus();
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, second);
});

test('overlay getter excludes hidden and removed entries rather than inventing an overlay', () => {
  const tui = new RemoteTUI(surface());
  const first = component(), second = component();
  const firstHandle = tui.facade.showOverlay(first);
  const secondHandle = tui.facade.showOverlay(second);
  secondHandle.setHidden(true);
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, first);
  secondHandle.setHidden(false);
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, second);
  secondHandle.hide();
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, first);
  firstHandle.hide();
  assert.equal(tui.facade.getTopmostVisibleOverlay(), undefined);
});

test('overlay getter uses actual current surface geometry and excludes noncapturing overlays', () => {
  const hostSurface = surface(), tui = new RemoteTUI(hostSurface);
  const capturing = component(), responsive = component(), hud = component();
  tui.facade.showOverlay(capturing);
  const sizes = [];
  tui.facade.showOverlay(responsive, { visible: (columns, rows) => { sizes.push([columns, rows]); return columns >= 100 && rows >= 30; } });
  tui.facade.showOverlay(hud, { nonCapturing: true });
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, capturing);
  assert.deepEqual(sizes.at(-1), [80, 24]);
  hostSurface.columns = 120; hostSurface.rows = 40;
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, responsive);
  assert.deepEqual(sizes.at(-1), [120, 40]);
  hostSurface.rows = 20;
  assert.equal(tui.facade.getTopmostVisibleOverlay().component, capturing);
});

test('overlay getter reports only its own maintained surface overlays', () => {
  const left = new RemoteTUI(surface()), right = new RemoteTUI(surface());
  const foreign = component();
  right.facade.showOverlay(foreign);
  assert.equal(right.facade.getTopmostVisibleOverlay().component, foreign);
  assert.equal(left.facade.getTopmostVisibleOverlay(), undefined);
  left.facade.showOverlay(component(), { nonCapturing: true });
  assert.equal(left.facade.hasOverlay(), true);
  assert.equal(left.facade.getTopmostVisibleOverlay(), undefined);
});
