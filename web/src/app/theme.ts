import { effect } from '@preact/signals-core';
import { THEMES, type ThemePref, theme } from '@/stores/prefs';

/**
 * DaisyUI themes are chosen through `html[data-theme]`: `light` or `dark` force a theme, and no
 * attribute follows the operating system (`prefers-color-scheme`, see `styles.css`).
 */
const THEME_ATTRIBUTE = 'data-theme';

export const THEME_LABELS: Readonly<Record<ThemePref, string>> = {
  system: 'System',
  light: 'Light',
  dark: 'Dark',
};

/** The preference after `pref` in the cycle system -> light -> dark -> system. */
export function nextTheme(pref: ThemePref): ThemePref {
  return THEMES[(THEMES.indexOf(pref) + 1) % THEMES.length] ?? 'system';
}

export function applyTheme(pref: ThemePref, root: HTMLElement = document.documentElement): void {
  if (pref === 'system') root.removeAttribute(THEME_ATTRIBUTE);
  else root.setAttribute(THEME_ATTRIBUTE, pref);
}

/**
 * Keeps `<html data-theme>` in step with the `theme` preference, now and on every change (also
 * from another tab). Returns a function that stops it. `public/theme-init.js` does the same
 * before the first paint; this takes over once the app is running.
 */
export function startTheme(root: HTMLElement = document.documentElement): () => void {
  return effect(() => applyTheme(theme.value, root));
}
