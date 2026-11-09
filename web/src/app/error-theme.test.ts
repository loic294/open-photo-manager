import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const styles = readFileSync('src/styles.css', 'utf8');
const themes = readFileSync('node_modules/daisyui/theme/object.js', 'utf8');

function luminance(hex: string): number {
  const channels = hex.match(/[0-9a-f]{2}/gi);
  if (channels?.length !== 3) throw new Error(`Invalid color: ${hex}`);
  const [red, green, blue] = channels.map((channel) => {
    const value = Number.parseInt(channel, 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

function contrast(first: number, second: number): number {
  return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
}

describe('light theme error tokens', () => {
  it('keeps error text and solid error content readable on the DaisyUI base surfaces', () => {
    const light = styles.match(/@plugin "daisyui\/theme"\s*\{([^}]*name:\s*"light"[^}]*)\}/)?.[1];
    expect(light).toBeDefined();
    const error = light?.match(/--color-error:\s*(#[0-9a-f]{6})\s*;/i)?.[1];
    const content = light?.match(/--color-error-content:\s*(#[0-9a-f]{6})\s*;/i)?.[1];
    if (!error || !content) throw new Error('Missing light theme error tokens');
    const errorLuminance = luminance(error);
    const builtin = JSON.parse(themes.replace(/^export default /, '')) as Record<
      string,
      Record<string, string>
    >;
    for (const surface of ['--color-base-100', '--color-base-200', '--color-base-300']) {
      const neutral = builtin.light[surface].match(/^oklch\(([\d.]+)% 0 0\)$/);
      if (!neutral) throw new Error(`Light ${surface} is no longer neutral OKLCH`);
      // Neutral OKLCH has linear sRGB luminance L cubed.
      expect(contrast(errorLuminance, (Number(neutral[1]) / 100) ** 3)).toBeGreaterThanOrEqual(4.5);
    }
    expect(contrast(errorLuminance, luminance(content))).toBeGreaterThanOrEqual(4.5);
  });
});
