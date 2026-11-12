import { render, type TemplateResult } from 'lit';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { contentUrl } from '@/api/files';
import {
  type ChromeActions,
  type ChromeView,
  renderInfoPanel,
  renderStepButtons,
  renderToolbar,
} from '@/components/viewer-chrome';
import { fileDetail, fileItem } from '@/test/file-item';

const hosts: HTMLElement[] = [];

function show(template: TemplateResult | unknown): HTMLElement {
  const host = document.createElement('div');
  document.body.append(host);
  hosts.push(host);
  render(template, host);
  return host;
}

function view(overrides: Partial<ChromeView> = {}): ChromeView {
  const item = fileItem({ id: 7, kind: 'image' });
  return {
    id: item.id,
    item,
    detail: undefined,
    idsStatus: 'ready',
    index: 2,
    count: 5,
    detailStatus: 'idle',
    zoomed: false,
    infoOpen: false,
    ...overrides,
  };
}

function actions() {
  return {
    toggleInfo: vi.fn<() => void>(),
    toggleZoom: vi.fn<() => void>(),
    step: vi.fn<ChromeActions['step']>(),
    retryDetail: vi.fn<() => void>(),
  };
}

const toolbar = (overrides?: Partial<ChromeView>, wired = actions()) =>
  show(renderToolbar(view(overrides), wired));
const control = <T extends HTMLElement = HTMLButtonElement>(
  host: HTMLElement,
  label: string,
): T => {
  const found = host.querySelector<T>(`[aria-label="${label}"]`);
  if (!found) throw new Error(`No control labelled "${label}"`);
  return found;
};
const text = (element: Element | null) => element?.textContent?.replace(/\s+/g, ' ').trim();

afterEach(() => {
  for (const host of hosts.splice(0)) host.remove();
});

describe('renderToolbar', () => {
  describe('the position', () => {
    it('is a skeleton while the list loads', () => {
      const host = toolbar({ idsStatus: 'loading' });

      expect(host.querySelector('[data-viewer-position]')?.classList.contains('skeleton')).toBe(
        true,
      );
      expect(host.querySelector('[data-viewer-notice]')).toBeNull();
    });

    it('reads "n / total" once the list is ready', () => {
      const host = toolbar({ index: 2, count: 340 });

      expect(text(host.querySelector('[data-viewer-position]'))).toBe('3 / 340');
    });

    it.each([
      ['failed', "Couldn't load the list"],
      ['missing', 'Not in the list anymore'],
    ] as const)('says why there is none when the list is %s', (idsStatus, message) => {
      const host = toolbar({ idsStatus });

      expect(text(host.querySelector('[data-viewer-notice]'))).toBe(message);
      expect(host.querySelector('[data-viewer-position]')).toBeNull();
      expect(host.querySelector('[data-viewer-notice]')?.classList.contains('badge-warning')).toBe(
        true,
      );
    });
  });

  describe('the info toggle', () => {
    it.each([false, true])('reflects the panel being open (%s)', (infoOpen) => {
      const button = control(toolbar({ infoOpen }), 'Info');

      expect(button.getAttribute('aria-pressed')).toBe(String(infoOpen));
      expect(button.classList.contains('btn-active')).toBe(infoOpen);
      expect(button.title).toBe('Info (I)');
    });

    it('asks to toggle', () => {
      const wired = actions();

      control(toolbar({}, wired), 'Info').click();

      expect(wired.toggleInfo).toHaveBeenCalledTimes(1);
    });
  });

  describe('the zoom toggle', () => {
    it('is for an image: enabled, and it offers to zoom in', () => {
      const button = control(toolbar(), '100% zoom');

      expect(button.disabled).toBe(false);
      expect(button.getAttribute('aria-pressed')).toBe('false');
      expect(button.querySelector('opm-icon')?.getAttribute('name')).toBe('zoom-in');
    });

    it('shows it is on, and offers to zoom out', () => {
      const button = control(toolbar({ zoomed: true }), '100% zoom');

      expect(button.getAttribute('aria-pressed')).toBe('true');
      expect(button.classList.contains('btn-active')).toBe(true);
      expect(button.querySelector('opm-icon')?.getAttribute('name')).toBe('zoom-out');
    });

    it.each(['video', 'audio', 'other'] as const)('is disabled, and never on, for a %s', (kind) => {
      const button = control(toolbar({ item: fileItem({ kind }), zoomed: true }), '100% zoom');

      expect(button.disabled).toBe(true);
      expect(button.getAttribute('aria-pressed')).toBe('false');
    });

    it('is disabled while the file is not known yet, but still there', () => {
      expect(control(toolbar({ item: undefined }), '100% zoom').disabled).toBe(true);
    });

    it('asks to toggle', () => {
      const wired = actions();

      control(toolbar({}, wired), '100% zoom').click();

      expect(wired.toggleZoom).toHaveBeenCalledTimes(1);
    });
  });

  describe('the links to the original', () => {
    it('open it in a new tab and download it, for the file asked for even before its row is known', () => {
      const host = toolbar({ id: 42, item: undefined });

      const original = control<HTMLAnchorElement>(host, 'Open original');
      expect(original.getAttribute('href')).toBe(contentUrl(42));
      expect(original.getAttribute('target')).toBe('_blank');
      expect(original.getAttribute('rel')).toBe('noopener');
      const download = control<HTMLAnchorElement>(host, 'Download');
      expect(download.getAttribute('href')).toBe(contentUrl(42, { download: true }));
      expect(download.hasAttribute('download')).toBe(true);
    });

    it('are left out once the server says the file is gone', () => {
      const host = toolbar({ item: undefined, detailStatus: 'missing' });

      expect(host.querySelector('a')).toBeNull();
    });
  });
});

describe('renderStepButtons', () => {
  const steps = (overrides?: Partial<ChromeView>, wired = actions()) =>
    show(renderStepButtons(view(overrides), wired));

  it('has a previous and a next button, labelled and titled with their key', () => {
    const host = steps();

    expect(control(host, 'Previous file').title).toBe('Previous (←)');
    expect(control(host, 'Next file').title).toBe('Next (→)');
    expect(control(host, 'Previous file').disabled).toBe(false);
    expect(control(host, 'Next file').disabled).toBe(false);
  });

  it('asks to step in the direction of the button', () => {
    const wired = actions();
    const host = steps({}, wired);

    control(host, 'Next file').click();
    control(host, 'Previous file').click();

    expect(wired.step.mock.calls).toEqual([['next'], ['previous']]);
  });

  it('disables previous at the first file and next at the last, and nothing else', () => {
    const first = steps({ index: 0 });
    expect(control(first, 'Previous file').disabled).toBe(true);
    expect(control(first, 'Next file').disabled).toBe(false);

    const last = steps({ index: 4 });
    expect(control(last, 'Previous file').disabled).toBe(false);
    expect(control(last, 'Next file').disabled).toBe(true);
  });

  it.each([
    ['the list is loading', { idsStatus: 'loading' }],
    ['the list could not be loaded', { idsStatus: 'failed' }],
    ['the file is not in the list', { idsStatus: 'missing' }],
    ['there is only one file', { count: 1, index: 0 }],
  ] as const)('has no buttons when %s', (_when, overrides) => {
    expect(steps(overrides).querySelector('button')).toBeNull();
  });
});

describe('renderInfoPanel', () => {
  const panel = (overrides?: Partial<ChromeView>, wired = actions()) =>
    show(renderInfoPanel(view({ infoOpen: true, ...overrides }), wired));

  it('is absent while closed', () => {
    expect(
      show(renderInfoPanel(view({ infoOpen: false }), actions())).querySelector('aside'),
    ).toBeNull();
  });

  it('is a labelled region with the file table', () => {
    const item = fileItem({ id: 7, name: 'sunset.jpg' });
    const host = panel({ item, detail: fileDetail(item) });

    expect(host.querySelector('aside')?.getAttribute('aria-label')).toBe('File information');
    expect(text(host.querySelector('[data-info-row="name"]'))).toContain('sunset.jpg');
    expect(host.querySelector('[data-info-alert]')).toBeNull();
  });

  it('shows the table from the listing row alone, before the detail arrives', () => {
    const host = panel({ item: fileItem({ name: 'row.jpg' }), detailStatus: 'pending' });

    expect(text(host.querySelector('[data-info-row="name"]'))).toContain('row.jpg');
  });

  it('keeps the table and adds a Retry when only the detail failed', () => {
    const wired = actions();
    const host = panel({ detailStatus: 'failed' }, wired);

    expect(host.querySelector('[data-info-row="name"]')).not.toBeNull();
    const alert = host.querySelector<HTMLElement>('[data-info-alert]');
    expect(text(alert)).toContain('Some details could not be loaded');
    alert?.querySelector('button')?.click();
    expect(wired.retryDetail).toHaveBeenCalledTimes(1);
  });

  it('says so when the file is gone', () => {
    const host = panel({ item: undefined, detailStatus: 'missing' });

    expect(text(host.querySelector('[data-info-missing]'))).toBe(
      'This file is no longer available',
    );
    expect(host.querySelector('[data-info-row]')).toBeNull();
  });

  it('offers Retry when nothing about the file could be loaded', () => {
    const wired = actions();
    const host = panel({ item: undefined, detailStatus: 'failed' }, wired);

    const alert = host.querySelector<HTMLElement>('[data-info-alert]');
    expect(alert?.classList.contains('alert-error')).toBe(true);
    expect(text(alert)).toContain("The file's details could not be loaded");
    alert?.querySelector('button')?.click();
    expect(wired.retryDetail).toHaveBeenCalledTimes(1);
  });

  it.each(['idle', 'pending'] as const)(
    'is a skeleton while an unknown file loads (%s)',
    (detailStatus) => {
      const host = panel({ item: undefined, detailStatus });

      expect(host.querySelector('[data-info-pending]')).not.toBeNull();
      expect(host.querySelector('[data-info-row]')).toBeNull();
    },
  );
});
