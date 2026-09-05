// Geometry contract: docs/assets/octet/manifest.json (01101111, eight positions).
// Product colors retain the native splash gradient and model-accent blend.
export const TUI_SPLASH_DURATION_SECONDS = 2.2;
export const TUI_SPLASH_WIDTH = 8;
export const TUI_SPLASH_ROWS = 2;
export const TUI_SPLASH_BITS = "01101111";

type Rgb = readonly [number, number, number];

export type TuiSplashCell = {
  glyph: string;
  color: string | null;
};

const GRADIENT_STOPS: readonly Rgb[] = [
  [0x4b, 0x8d, 0xff],
  [0x45, 0xd9, 0xe8],
  [0x54, 0xe6, 0xb5],
  [0x8d, 0xff, 0x6a],
];

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function mix(first: Rgb, second: Rgb, amount: number): Rgb {
  const channel = (left: number, right: number) =>
    left + (right - left) * amount;
  return [
    channel(first[0], second[0]),
    channel(first[1], second[1]),
    channel(first[2], second[2]),
  ];
}

function gradient(
  position: number,
  lift: number,
  modelAccent: Rgb,
): Rgb {
  const progress = clamp(position, 0, 1) * 3;
  const index = Math.min(2, Math.floor(progress));
  const base = mix(
    GRADIENT_STOPS[index]!,
    GRADIENT_STOPS[index + 1]!,
    progress - index,
  );
  const adapted = mix(base, modelAccent, 0.58);
  return mix(adapted, [255, 255, 255], lift);
}

function parseHexColor(source: string): Rgb | null {
  const match = /^#([\da-f]{2})([\da-f]{2})([\da-f]{2})$/i.exec(source);
  if (!match) return null;
  return [
    Number.parseInt(match[1]!, 16),
    Number.parseInt(match[2]!, 16),
    Number.parseInt(match[3]!, 16),
  ];
}

function relativeLuminance(color: Rgb) {
  const channels = color.map((channel) => {
    const value = channel / 255;
    return value <= 0.04045
      ? value / 12.92
      : ((value + 0.055) / 1.055) ** 2.4;
  });
  return (
    channels[0]! * 0.2126 +
    channels[1]! * 0.7152 +
    channels[2]! * 0.0722
  );
}

function balanceForeground(source: Rgb, target: number): Rgb {
  const luminance = relativeLuminance(source);
  if (Math.abs(luminance - target) <= 0.002) return source;
  const destination: Rgb =
    luminance < target ? [255, 255, 255] : [0, 0, 0];
  let low = 0;
  let high = 1;
  for (let iteration = 0; iteration < 20; iteration += 1) {
    const amount = (low + high) / 2;
    const candidate = mix(source, destination, amount);
    const reached =
      luminance < target
        ? relativeLuminance(candidate) >= target
        : relativeLuminance(candidate) <= target;
    if (reached) high = amount;
    else low = amount;
  }
  return mix(source, destination, high);
}

function colorString(color: Rgb) {
  return `rgb(${color.map((channel) => Math.trunc(clamp(channel, 0, 255))).join(" ")})`;
}

/** Complete byte from the first frame; optional shimmer never changes geometry. */
export function renderTuiSplashFrame(
  elapsed: number,
  modelAccentSource: string,
): { light: TuiSplashCell[]; dark: TuiSplashCell[] } {
  const source = parseHexColor(modelAccentSource) ?? [0x16, 0x87, 0x6d];
  const time = clamp(elapsed, 0, TUI_SPLASH_DURATION_SECONDS);
  const cells = (target: number): TuiSplashCell[] => {
    const accent = balanceForeground(source, target);
    return Array.from({ length: TUI_SPLASH_WIDTH * TUI_SPLASH_ROWS }, (_, index) => {
      const column = index % TUI_SPLASH_WIDTH;
      const filled = index >= TUI_SPLASH_WIDTH || TUI_SPLASH_BITS[column] === "1";
      const position = column / (TUI_SPLASH_WIDTH - 1);
      const front = (time - 1.2) / 0.55;
      const lift = time >= 1.2 && time < 1.75
        ? Math.exp(-(((position - front) / 0.13) ** 2)) * 0.2
        : 0;
      return {
        glyph: filled ? "█" : " ",
        color: filled ? colorString(gradient(position, lift, accent)) : null,
      };
    });
  };
  return { light: cells(0.11), dark: cells(0.27) };
}
