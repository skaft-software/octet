import { invalid } from './errors.mjs';
// Rebuild legacy presses (important for unchanged handlers comparing data === 'q')
// and Kitty event-type sequences for modifiers, repeats and key releases.
export function keyData({ key, kind = 'press', modifiers = [] }) {
  if (!['press', 'repeat', 'release'].includes(kind) || !Array.isArray(modifiers) || new Set(modifiers).size !== modifiers.length || modifiers.some(m => !['shift', 'alt', 'control', 'super'].includes(m))) invalid('ui/key event');
  const bits = { shift: 1, alt: 2, control: 4, super: 8 };
  const modifier = 1 + modifiers.reduce((n, m) => n + bits[m], 0);
  const type = kind === 'release' ? 3 : kind === 'repeat' ? 2 : 1;
  const arrow = { ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D', Home: 'H', End: 'F' }[key];
  if (arrow) return modifier === 1 && type === 1 ? `\x1b[${arrow}` : `\x1b[1;${modifier}:${type}${arrow}`;
  const functional = { Insert: 2, Delete: 3, PageUp: 5, PageDown: 6 }[key];
  if (functional) return `\x1b[${functional};${modifier}:${type}~`;
  const named = { Enter: 13, Escape: 27, Tab: 9, Space: 32, Backspace: 127 };
  const code = named[key] ?? (typeof key === 'string' && [...key].length === 1 ? key.codePointAt(0) : undefined);
  if (code === undefined) {
    const f = /^F([1-9]|1[0-2])$/.exec(key);
    if (f) return `\x1b[${57364 + Number(f[1]) - 1};${modifier}:${type}u`;
    invalid(`unrecognized ui/key ${key}`);
  }
  if (type === 1 && modifier === 1) return String.fromCodePoint(code);
  if (type === 1 && modifiers.length === 1 && modifiers[0] === 'shift' && code >= 97 && code <= 122) return String.fromCodePoint(code - 32);
  if (type === 1 && modifiers.length === 1 && modifiers[0] === 'control' && code >= 97 && code <= 122) return String.fromCodePoint(code - 96);
  return `\x1b[${code};${modifier}:${type}u`;
}
export function mouseData({ kind, button, x, y, modifiers = [], wheel_delta = 0 }) {
  if (!['press', 'release', 'drag', 'move', 'wheel'].includes(kind) || !['left', 'middle', 'right', 'none'].includes(button) || !Number.isInteger(x) || x < 0 || x > 65535 || !Number.isInteger(y) || y < 0 || y > 65535 || !Number.isInteger(wheel_delta) || wheel_delta < -32768 || wheel_delta > 32767 || !Array.isArray(modifiers) || modifiers.some(m => !['shift', 'alt', 'control', 'super'].includes(m))) invalid('ui/mouse');
  if (modifiers.includes('super')) invalid('SGR mouse cannot represent super modifier');
  let code = { left: 0, middle: 1, right: 2, none: 3 }[button];
  if (kind === 'drag' || kind === 'move') code |= 32;
  if (kind === 'wheel') code = 64 + (wheel_delta > 0 ? 1 : 0);
  if (modifiers.includes('shift')) code |= 4;
  if (modifiers.includes('alt')) code |= 8;
  if (modifiers.includes('control')) code |= 16;
  return `\x1b[<${code};${x + 1};${y + 1}${kind === 'release' ? 'm' : 'M'}`;
}
export function reservedKey({ key, modifiers = [] }, placement) {
  if (!modifiers.includes('control') || typeof key !== 'string') return false;
  const normalized = key.toLowerCase();
  return normalized === 'd' || normalized === 'g' && placement === 'fullscreen';
}
