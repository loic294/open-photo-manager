import { describe, expect, it } from 'vitest';
import indexHtml from '../index.html?raw';

/**
 * The server sends `script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'` (see
 * AGENTS.md, "Content Security Policy"). These checks catch what the policy would block in the
 * browser, where nobody runs the production build under it during development.
 */

const publicFiles = import.meta.glob<string>('/public/*', {
  query: '?raw',
  import: 'default',
  eager: true,
});

const appSources = Object.entries(
  import.meta.glob<string>(['/src/**/*.ts', '!/src/**/*.test.ts', '!/src/api/generated/**'], {
    query: '?raw',
    import: 'default',
    eager: true,
  }),
);

const doc = new DOMParser().parseFromString(indexHtml, 'text/html');

describe('index.html', () => {
  it('has no inline script, inline style or inline event handler', () => {
    const scripts = [...doc.querySelectorAll('script')];
    expect(scripts.length).toBeGreaterThan(0);
    for (const script of scripts) {
      expect(script.getAttribute('src'), 'every script has a src').toBeTruthy();
      expect(script.textContent?.trim()).toBe('');
    }
    expect(doc.querySelectorAll('style')).toHaveLength(0);

    const handlers = [...doc.querySelectorAll('*')]
      .flatMap((element) => element.getAttributeNames())
      .filter((name) => name.startsWith('on'));
    expect(handlers).toEqual([]);
  });

  it('loads everything from its own origin', () => {
    const urls = [...doc.querySelectorAll('script[src], link[href]')].map(
      (element) => element.getAttribute('src') ?? element.getAttribute('href') ?? '',
    );
    expect(urls.length).toBeGreaterThan(0);
    for (const url of urls) {
      expect(url, 'a root-relative path').toMatch(/^\/(?!\/)/);
    }
  });

  it('serves every public file it links from public/', () => {
    const urls = [...doc.querySelectorAll('script[src], link[href]')]
      .map((element) => element.getAttribute('src') ?? element.getAttribute('href') ?? '')
      .filter((url) => !url.startsWith('/src/'));
    expect(urls).toContain('/favicon.svg');
    for (const url of urls) {
      expect(Object.keys(publicFiles), url).toContain(`/public${url}`);
    }
  });

  it('applies the stored theme with a blocking classic script before the app starts', () => {
    const scripts = [...doc.querySelectorAll('script')];
    const init = scripts.find((script) => script.getAttribute('src') === '/theme-init.js');
    const entry = scripts.find((script) => script.getAttribute('type') === 'module');

    expect(init, 'theme-init.js is linked').toBeDefined();
    expect(init?.parentElement?.localName).toBe('head');
    expect(init?.hasAttribute('type')).toBe(false);
    expect(init?.hasAttribute('async')).toBe(false);
    expect(init?.hasAttribute('defer')).toBe(false);
    expect(entry?.getAttribute('src')).toBe('/src/main.ts');
    expect(scripts.indexOf(init as HTMLScriptElement)).toBeLessThan(
      scripts.indexOf(entry as HTMLScriptElement),
    );
  });

  it('puts the app shell in the body', () => {
    expect([...doc.body.children].map((element) => element.localName)).toEqual([
      'opm-app',
      'script',
    ]);
    expect(doc.documentElement.lang).toBe('en');
  });
});

describe('app sources', () => {
  it('finds the sources it checks', () => {
    expect(appSources.length).toBeGreaterThan(20);
  });

  it.each([
    ['inline <style> or <script> elements', /<(style|script)[\s>]/i],
    ['eval or the Function constructor', /\beval\s*\(|\bnew\s+Function\s*\(/],
    ['javascript: URLs', /javascript:/i],
    ['inline on* event handler attributes', /\son[a-z]+=["'$]/i],
    ['string arguments to timers', /set(Timeout|Interval)\(\s*["'`]/],
  ])('has no %s', (_name, pattern) => {
    const offenders = appSources.filter(([, source]) => pattern.test(source)).map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});
