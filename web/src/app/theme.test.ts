import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { applyTheme, nextTheme, startTheme, THEME_LABELS } from '@/app/theme';
import { STORAGE_PREFIX } from '@/stores/persisted';
import { THEMES, theme } from '@/stores/prefs';
import themeInit from '../../public/theme-init.js?raw';

const root = document.documentElement;

function dataTheme(): string | null {
  return root.getAttribute('data-theme');
}

/** Runs public/theme-init.js the way the browser does: as a classic script in the page. */
function runThemeInit(): void {
  new Function(themeInit)();
}

beforeEach(() => {
  theme.value = 'system';
  localStorage.clear();
  root.removeAttribute('data-theme');
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('nextTheme', () => {
  it('cycles system, light, dark and back to system', () => {
    expect(nextTheme('system')).toBe('light');
    expect(nextTheme('light')).toBe('dark');
    expect(nextTheme('dark')).toBe('system');
  });

  it('has a label for every preference', () => {
    expect(THEMES.map((pref) => THEME_LABELS[pref])).toEqual(['System', 'Light', 'Dark']);
  });
});

describe('applyTheme', () => {
  it('forces light or dark through html[data-theme]', () => {
    applyTheme('dark');
    expect(dataTheme()).toBe('dark');

    applyTheme('light');
    expect(dataTheme()).toBe('light');
  });

  it('removes the attribute for system, so DaisyUI follows prefers-color-scheme', () => {
    applyTheme('dark');

    applyTheme('system');

    expect(root.hasAttribute('data-theme')).toBe(false);
  });

  it('can target another element', () => {
    const other = document.createElement('div');

    applyTheme('dark', other);

    expect(other.getAttribute('data-theme')).toBe('dark');
    expect(dataTheme()).toBeNull();
  });
});

describe('startTheme', () => {
  it('applies the current preference at once and follows every change', () => {
    theme.value = 'dark';
    const stop = startTheme();
    expect(dataTheme()).toBe('dark');

    theme.value = 'light';
    expect(dataTheme()).toBe('light');

    theme.value = 'system';
    expect(root.hasAttribute('data-theme')).toBe(false);
    stop();
  });

  it('follows a change made in another tab', () => {
    const stop = startTheme();

    window.dispatchEvent(
      new StorageEvent('storage', {
        key: `${STORAGE_PREFIX}theme`,
        newValue: '"dark"',
        storageArea: localStorage,
      }),
    );

    expect(dataTheme()).toBe('dark');
    stop();
  });

  it('stops following once stopped', () => {
    const stop = startTheme();
    stop();

    theme.value = 'dark';

    expect(dataTheme()).toBeNull();
  });
});

describe('public/theme-init.js', () => {
  it.each(['light', 'dark'])('applies a stored %s theme', (stored) => {
    localStorage.setItem('apm.v1.theme', JSON.stringify(stored));

    runThemeInit();

    expect(dataTheme()).toBe(stored);
  });

  it('applies nothing for the system theme or for no stored choice', () => {
    runThemeInit();
    expect(dataTheme()).toBeNull();

    localStorage.setItem('apm.v1.theme', '"system"');
    runThemeInit();
    expect(dataTheme()).toBeNull();
  });

  it.each([
    ['an unknown theme', '"purple"'],
    ['something that is not a string', '42'],
    ['a stored null', 'null'],
    ['text that is not JSON', 'dark'],
    ['an empty value', ''],
  ])('ignores %s', (_, stored) => {
    localStorage.setItem('apm.v1.theme', stored);

    expect(runThemeInit).not.toThrow();
    expect(dataTheme()).toBeNull();
  });

  it('does not throw when storage is blocked', () => {
    vi.spyOn(window, 'localStorage', 'get').mockImplementation(() => {
      throw new DOMException('denied', 'SecurityError');
    });

    expect(runThemeInit).not.toThrow();
    expect(dataTheme()).toBeNull();
  });

  it('does not throw when reading fails', () => {
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('broken');
      },
    });

    expect(runThemeInit).not.toThrow();
    expect(dataTheme()).toBeNull();
  });

  it('reads what the theme preference writes (the storage key is shared)', () => {
    for (const pref of ['dark', 'light'] as const) {
      root.removeAttribute('data-theme');
      theme.value = pref;
      expect(localStorage.getItem(`${STORAGE_PREFIX}theme`)).toBe(JSON.stringify(pref));

      runThemeInit();

      expect(dataTheme()).toBe(pref);
    }
  });
});
