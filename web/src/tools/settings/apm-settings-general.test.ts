import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CSRF_HEADER } from '@/api/client';
import type { SettingsDto } from '@/api/generated/SettingsDto';
import type { ApmChipInput } from '@/components/apm-chip-input';
import { clearToasts, toasts } from '@/stores/toasts';
import { mount } from '@/test/helpers';
import { sampleSettings } from '@/test/settings-fixtures';
import '@/tools/settings/apm-settings-general';

interface Reply {
  status?: number;
  body?: unknown;
  /** Holds the answer back until it settles. */
  gate?: Promise<unknown>;
}

interface Sent {
  method: string;
  path: string;
  body: unknown;
  headers: Headers;
}

const ok = (body: unknown): Reply => ({ body });
const problem = (status: number, code: string, message: string, fields?: unknown): Reply => ({
  status,
  body: { error: { code, message, ...(fields ? { fields } : {}) } },
});
const refused = (...fields: { field: string; message: string }[]): Reply =>
  problem(422, 'validation_failed', 'the settings were refused', fields);

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/** Answers the next requests with `replies`, in order, and records what was sent. */
function serve(...replies: Reply[]): Sent[] {
  const queue = [...replies];
  const sent: Sent[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      sent.push({
        method: init?.method ?? 'GET',
        path: String(input),
        body: typeof init?.body === 'string' ? JSON.parse(init.body) : undefined,
        headers: new Headers(init?.headers),
      });
      const reply = queue.shift();
      if (!reply) throw new Error(`Unexpected request: ${init?.method} ${String(input)}`);
      await reply.gate;
      return new Response(JSON.stringify(reply.body), {
        status: reply.status ?? 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
  return sent;
}

type Form = Awaited<ReturnType<typeof open>>['element'];

/** Mounts the form on a server that has `doc`, and waits until the document has loaded. */
async function open(doc: SettingsDto = sampleSettings(), ...next: Reply[]) {
  const requests = serve(ok(doc), ...next);
  const element = await mount('apm-settings-general');
  await vi.waitFor(() => expect(element.querySelector('form')).not.toBeNull());
  await element.updateComplete;
  return { element, requests };
}

const field = <T extends HTMLElement = HTMLInputElement>(element: Form, name: string): T => {
  const found = element.querySelector<T>(`[data-field="${name}"]`);
  if (!found) throw new Error(`No field ${name}`);
  return found;
};
const chips = (element: Form, name: string) => field<ApmChipInput>(element, name);
const byId = <T extends HTMLElement = HTMLInputElement>(element: Form, id: string): T => {
  const found = element.querySelector<T>(`#settings-general-${id}`);
  if (!found) throw new Error(`No element #${id}`);
  return found;
};
const buttonLabelled = (element: Form, label: string) =>
  [...element.querySelectorAll('button')].find(
    (button) => button.textContent?.replace(/\s+/g, ' ').trim() === label,
  );
const save = (element: Form) => buttonLabelled(element, 'Save') as HTMLButtonElement;
const discard = (element: Form) => buttonLabelled(element, 'Discard') as HTMLButtonElement;
const bar = (element: Form) => element.querySelector('.navbar');
const alertOf = (element: Form) => element.querySelector('form [role="alert"].alert');
const messageOf = (element: Form, name: string) => {
  const id = field(element, name).getAttribute('aria-describedby') ?? '';
  return element.querySelector(`#${id}`) as HTMLElement;
};

async function typeNumber(element: Form, name: string, text: string): Promise<void> {
  const input = field(element, name);
  input.value = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await element.updateComplete;
}

async function flip(element: Form, id: string, checked: boolean): Promise<void> {
  const input = byId(element, id);
  input.checked = checked;
  input.dispatchEvent(new Event('change', { bubbles: true }));
  await element.updateComplete;
}

async function choose(element: Form, value: string): Promise<void> {
  const select = byId<HTMLSelectElement>(element, 'conflict-policy');
  select.value = value;
  select.dispatchEvent(new Event('change', { bubbles: true }));
  await element.updateComplete;
}

async function addEntry(element: Form, name: string, text: string): Promise<void> {
  const list = chips(element, name);
  const box = list.querySelector('input') as HTMLInputElement;
  box.value = text;
  box.dispatchEvent(
    new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
  );
  await list.updateComplete;
  await element.updateComplete;
}

async function removeEntry(element: Form, name: string, entry: string): Promise<void> {
  const list = chips(element, name);
  list.querySelector<HTMLButtonElement>(`button[aria-label="Remove ${entry}"]`)?.click();
  await list.updateComplete;
  await element.updateComplete;
}

/** Makes the form differ from the server's document. */
const makeDirty = (element: Form) => typeNumber(element, 'name_max_len', '200');

async function pressSave(element: Form): Promise<void> {
  save(element).click();
  await element.updateComplete;
}

const toastTexts = () => toasts.value.map((toast) => `${toast.kind}: ${toast.message}`);

function leave(): boolean {
  const event = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(event);
  return event.defaultPrevented;
}

beforeEach(() => {
  clearToasts();
});

afterEach(() => {
  clearToasts();
  vi.unstubAllGlobals();
});

describe('apm-settings-general', () => {
  describe('loading', () => {
    it('shows a skeleton until the settings arrive', async () => {
      const gate = deferred();
      serve({ ...ok(sampleSettings()), gate: gate.promise });
      const element = await mount('apm-settings-general');

      expect(element.querySelector('[role="status"]')?.getAttribute('aria-label')).toBe(
        'Loading the settings',
      );
      expect(element.querySelectorAll('.skeleton')).toHaveLength(3);
      expect(element.querySelector('form')).toBeNull();

      gate.resolve();
      await vi.waitFor(() => expect(element.querySelector('form')).not.toBeNull());
      expect(element.querySelector('.skeleton')).toBeNull();
    });

    it('asks the server for the settings once', async () => {
      const { requests } = await open();

      expect(requests).toHaveLength(1);
      expect(requests[0]).toMatchObject({ method: 'GET', path: '/api/settings' });
    });

    it('is a block', async () => {
      const { element } = await open();

      expect(element.classList.contains('block')).toBe(true);
    });

    it('says why the settings could not load, and loads them again on Retry', async () => {
      serve(problem(503, 'db_busy', 'The database is busy'), ok(sampleSettings()));
      const element = await mount('apm-settings-general');
      await vi.waitFor(() => expect(element.querySelector('.alert')).not.toBeNull());

      expect(element.querySelector('.alert')?.textContent).toContain('The database is busy');
      expect(element.querySelector('form')).toBeNull();

      buttonLabelled(element, 'Retry')?.click();
      await vi.waitFor(() => expect(element.querySelector('form')).not.toBeNull());
      expect(element.querySelector('.alert')).toBeNull();
    });
  });

  describe('the loaded document', () => {
    it('is shown in three cards', async () => {
      const { element } = await open();

      const legends = [...element.querySelectorAll('fieldset > legend')].map((legend) =>
        legend.textContent?.trim(),
      );
      expect(legends).toEqual(['Files and sidecars', 'Moving and deleting', 'Indexing']);
      expect(element.querySelector('form')?.getAttribute('aria-label')).toBe('General settings');
    });

    it('fills the lists and numbers', async () => {
      const doc = sampleSettings({ name_max_len: 143 });
      const { element } = await open(doc);

      expect(chips(element, 'sidecar_extensions').values).toEqual(doc.sidecar_extensions);
      expect(chips(element, 'junk_names').values).toEqual(doc.junk_names);
      expect(chips(element, 'junk_prefixes').values).toEqual(['._']);
      expect(field(element, 'name_max_len').value).toBe('143');
      expect(field(element, 'index.concurrency').value).toBe('2');
      expect(field(element, 'index.hash_concurrency').value).toBe('1');
      expect(field(element, 'index.thumbnail_concurrency').value).toBe('2');
      expect(field(element, 'index.rescan_interval_minutes').value).toBe('360');
    });

    it('labels every field and tells the list fields to allow spaces only in names', async () => {
      const { element } = await open();

      const labels = [...element.querySelectorAll('label')].map((label) =>
        label.textContent?.trim(),
      );
      expect(labels).toEqual(
        expect.arrayContaining([
          'Sidecar extensions',
          'Ignored names',
          'Ignored prefixes',
          'Longest generated name',
          'When a file with the same name already exists',
          'Keep the original in the trash after moving a file to another volume',
          'Delete trashed files automatically',
          'Delete after',
          'Scan workers',
          'Hash workers',
          'Thumbnail workers',
          'Watch for changes',
          'Full rescan interval',
        ]),
      );
      expect(chips(element, 'sidecar_extensions').allowSpaces).toBe(false);
      expect(chips(element, 'junk_names').allowSpaces).toBe(true);
      expect(chips(element, 'junk_prefixes').allowSpaces).toBe(true);
    });

    it('points every number input at its message and its unit', async () => {
      const { element } = await open();

      for (const name of [
        'name_max_len',
        'trash_retention_days',
        'index.concurrency',
        'index.hash_concurrency',
        'index.thumbnail_concurrency',
        'index.rescan_interval_minutes',
      ]) {
        expect(messageOf(element, name).textContent?.trim()).not.toBe('');
      }
      expect(field(element, 'name_max_len').parentElement?.textContent).toContain('bytes');
      expect(field(element, 'index.rescan_interval_minutes').parentElement?.textContent).toContain(
        'minutes',
      );
    });

    it('notes that the lists apply to the next scan and the workers to the next batch', async () => {
      const { element } = await open();

      const text = element.textContent ?? '';
      expect(text).toContain(
        'Applies to the next scan. Use Re-index in the menu to apply it to files that are already indexed.',
      );
      expect(text).toContain('Low values are right for a NAS CPU');
      expect(text).toContain('Encrypted shares only allow 143');
    });

    it('selects the default conflict policy and describes it', async () => {
      const { element } = await open(sampleSettings({ default_conflict_policy: 'replace' }));

      const select = byId<HTMLSelectElement>(element, 'conflict-policy');
      expect(select.value).toBe('replace');
      expect([...select.options].map((option) => option.value)).toEqual([
        'smart',
        'skip',
        'rename',
        'replace',
      ]);
      const help = element.querySelector(`#${select.getAttribute('aria-describedby')}`);
      expect(help?.textContent).toContain('goes to the trash');
    });

    it('sets the switches from the document', async () => {
      const { element } = await open(
        sampleSettings({
          keep_cross_volume_source_in_trash: true,
          trash_retention_days: 14,
          index: { ...sampleSettings().index, watch_enabled: false },
        }),
      );

      expect(byId(element, 'keep-cross-volume-source').checked).toBe(true);
      expect(byId(element, 'trash-auto-delete').checked).toBe(true);
      expect(byId(element, 'watch').checked).toBe(false);
      expect(byId(element, 'watch').getAttribute('role')).toBe('switch');
    });

    it('shows the days only as a suggestion while the automatic deletion is off', async () => {
      const { element } = await open();

      expect(byId(element, 'trash-auto-delete').checked).toBe(false);
      expect(field(element, 'trash_retention_days').disabled).toBe(true);
      expect(field(element, 'trash_retention_days').value).toBe('30');
    });

    it('enables the days while the automatic deletion is on', async () => {
      const { element } = await open(sampleSettings({ trash_retention_days: 14 }));

      expect(field(element, 'trash_retention_days').disabled).toBe(false);
      expect(field(element, 'trash_retention_days').value).toBe('14');
    });

    it('describes the rescan interval in words', async () => {
      const { element } = await open();

      expect(messageOf(element, 'index.rescan_interval_minutes').textContent).toContain(
        'Every 6 hours.',
      );
      await typeNumber(element, 'index.rescan_interval_minutes', '0');
      expect(messageOf(element, 'index.rescan_interval_minutes').textContent).toContain(
        'Never rescans on its own.',
      );
      await typeNumber(element, 'index.rescan_interval_minutes', '1500');
      expect(messageOf(element, 'index.rescan_interval_minutes').textContent).toContain(
        'Every 1 day 1 hour.',
      );
      await typeNumber(element, 'index.rescan_interval_minutes', '5');
      expect(messageOf(element, 'index.rescan_interval_minutes').textContent).toContain(
        'Every 5 minutes.',
      );
    });

    it('marks nothing as invalid', async () => {
      const { element } = await open();

      expect(element.querySelector('.validator')).toBeNull();
      expect(element.querySelector('[aria-invalid="true"]')).toBeNull();
      expect(alertOf(element)).toBeNull();
    });
  });

  describe('unsaved changes', () => {
    it('has no action bar while nothing changed', async () => {
      const { element } = await open();

      expect(bar(element)).toBeNull();
    });

    it('shows the bar with Save and Discard as soon as a field changes', async () => {
      const { element } = await open();

      await makeDirty(element);

      expect(bar(element)?.textContent).toContain('Unsaved changes');
      expect(save(element).disabled).toBe(false);
      expect(save(element).classList.contains('btn-primary')).toBe(true);
      expect(discard(element).disabled).toBe(false);
    });

    it('hides the bar again when the change is undone', async () => {
      const { element } = await open();

      await makeDirty(element);
      await typeNumber(element, 'name_max_len', '255');

      expect(bar(element)).toBeNull();
    });

    it('sees an edit of every kind of field', async () => {
      const { element } = await open();
      const edits: [string, () => Promise<void>][] = [
        ['a list', () => addEntry(element, 'sidecar_extensions', 'raf')],
        ['a number', () => typeNumber(element, 'index.concurrency', '4')],
        ['a switch', () => flip(element, 'watch', false)],
        ['the policy', () => choose(element, 'skip')],
      ];

      for (const [, edit] of edits) {
        await edit();
        expect(bar(element), 'bar').not.toBeNull();
        discard(element).click();
        await element.updateComplete;
        expect(bar(element), 'bar after discard').toBeNull();
      }
    });

    it('puts everything back on Discard', async () => {
      const doc = sampleSettings();
      const { element, requests } = await open(doc);
      await addEntry(element, 'junk_names', 'Backup');
      await typeNumber(element, 'name_max_len', '143');
      await flip(element, 'keep-cross-volume-source', true);
      await flip(element, 'trash-auto-delete', true);
      await choose(element, 'replace');

      discard(element).click();
      await element.updateComplete;

      expect(chips(element, 'junk_names').values).toEqual(doc.junk_names);
      expect(field(element, 'name_max_len').value).toBe('255');
      expect(byId(element, 'keep-cross-volume-source').checked).toBe(false);
      expect(byId(element, 'trash-auto-delete').checked).toBe(false);
      expect(byId<HTMLSelectElement>(element, 'conflict-policy').value).toBe('smart');
      expect(bar(element)).toBeNull();
      expect(requests).toHaveLength(1);
    });

    it('adds and removes list entries', async () => {
      const { element } = await open();

      await addEntry(element, 'sidecar_extensions', '.RAF');
      await removeEntry(element, 'sidecar_extensions', 'xmp');

      expect(chips(element, 'sidecar_extensions').values).toEqual([
        'json',
        'aae',
        'dop',
        'pp3',
        'xml',
        'raf',
      ]);
      expect(bar(element)).not.toBeNull();
    });

    it('keeps Enter in a chip box from submitting the form', async () => {
      const { element, requests } = await open();
      const submitted = vi.fn((event: Event) => event.preventDefault());
      element.querySelector('form')?.addEventListener('submit', submitted);

      await addEntry(element, 'sidecar_extensions', 'raf');

      expect(submitted).not.toHaveBeenCalled();
      expect(requests).toHaveLength(1);
    });

    it('never lets a submit navigate the page', async () => {
      const { element, requests } = await open();
      await makeDirty(element);
      const submit = new Event('submit', { bubbles: true, cancelable: true });

      element.querySelector('form')?.dispatchEvent(submit);

      expect(submit.defaultPrevented).toBe(true);
      expect(requests).toHaveLength(1);
    });
  });

  describe('number checks', () => {
    const cases: [string, string, string, string[]][] = [
      ['name_max_len', 'Longest generated name', '15', ['16', '255']],
      ['trash_retention_days', 'Delete after', '0', ['1', '3650']],
      ['index.concurrency', 'Scan workers', '17', ['1', '16']],
      ['index.hash_concurrency', 'Hash workers', '9', ['1', '8']],
      ['index.thumbnail_concurrency', 'Thumbnail workers', '0', ['1', '8']],
      ['index.rescan_interval_minutes', 'Full rescan interval', '4', ['0', '5', '10080']],
    ];

    it.each(cases)(
      'rejects %s outside the limits and accepts the limits',
      async (name, _l, bad, good) => {
        const { element } = await open(sampleSettings({ trash_retention_days: 30 }));
        const original = field(element, name).value;

        await typeNumber(element, name, bad);
        expect(field(element, name).getAttribute('aria-invalid'), `${bad} aria-invalid`).toBe(
          'true',
        );
        expect(field(element, name).parentElement?.classList.contains('validator')).toBe(true);
        expect(messageOf(element, name).getAttribute('role')).toBe('alert');
        expect(messageOf(element, name).classList.contains('validator-hint')).toBe(true);
        expect(save(element).disabled).toBe(true);
        expect(bar(element)?.textContent).toContain('Some values are not valid');

        for (const value of good) {
          await typeNumber(element, name, value);
          expect(field(element, name).hasAttribute('aria-invalid'), `${value} is valid`).toBe(
            false,
          );
          expect(messageOf(element, name).getAttribute('role')).toBeNull();
          expect(messageOf(element, name).classList.contains('validator-hint')).toBe(false);
          if (value !== original) {
            expect(save(element).disabled, `Save with ${value}`).toBe(false);
          } else {
            expect(bar(element), `no bar with the original ${value}`).toBeNull();
          }
        }
      },
    );

    it('rejects an empty box, a fraction and a negative number', async () => {
      const { element } = await open();

      for (const text of ['', '1.5', '-3']) {
        await typeNumber(element, 'index.concurrency', text);
        expect(field(element, 'index.concurrency').getAttribute('aria-invalid'), text).toBe('true');
      }
    });

    it('says what is expected with the field', async () => {
      const { element } = await open();

      await typeNumber(element, 'name_max_len', '10');
      expect(messageOf(element, 'name_max_len').textContent?.trim()).toBe(
        'Enter a whole number from 16 to 255.',
      );

      await typeNumber(element, 'index.rescan_interval_minutes', '2');
      expect(messageOf(element, 'index.rescan_interval_minutes').textContent?.trim()).toBe(
        'Enter 0 to never rescan, or a whole number of minutes from 5 to 10080.',
      );
    });

    it('does not send a document with an invalid number', async () => {
      const { element, requests } = await open();
      await typeNumber(element, 'name_max_len', '10');

      expect(save(element).disabled).toBe(true);
      await pressSave(element);

      expect(requests).toHaveLength(1);
    });

    it('does not check the days while the automatic deletion is off', async () => {
      const { element } = await open();

      await makeDirty(element);

      expect(field(element, 'trash_retention_days').hasAttribute('aria-invalid')).toBe(false);
      expect(save(element).disabled).toBe(false);
    });

    it('lets Discard clear an invalid number', async () => {
      const { element } = await open();
      await typeNumber(element, 'name_max_len', '10');

      discard(element).click();
      await element.updateComplete;

      expect(field(element, 'name_max_len').value).toBe('255');
      expect(element.querySelector('[aria-invalid="true"]')).toBeNull();
    });
  });

  describe('trash retention', () => {
    it('sends null when the automatic deletion is turned off', async () => {
      const { element, requests } = await open(
        sampleSettings({ trash_retention_days: 14 }),
        ok(sampleSettings()),
      );

      await flip(element, 'trash-auto-delete', false);
      expect(field(element, 'trash_retention_days').disabled).toBe(true);
      await pressSave(element);
      await vi.waitFor(() => expect(requests).toHaveLength(2));

      expect(requests[1]?.body).toMatchObject({ trash_retention_days: null });
    });

    it('starts at 30 days when it is turned on', async () => {
      const { element, requests } = await open(sampleSettings(), ok(sampleSettings()));

      await flip(element, 'trash-auto-delete', true);
      expect(field(element, 'trash_retention_days').disabled).toBe(false);
      expect(field(element, 'trash_retention_days').value).toBe('30');
      await pressSave(element);
      await vi.waitFor(() => expect(requests).toHaveLength(2));

      expect(requests[1]?.body).toMatchObject({ trash_retention_days: 30 });
    });

    it('turns on again with the days that were chosen last', async () => {
      const { element } = await open(sampleSettings({ trash_retention_days: 14 }));

      await typeNumber(element, 'trash_retention_days', '60');
      await flip(element, 'trash-auto-delete', false);
      await flip(element, 'trash-auto-delete', true);

      expect(field(element, 'trash_retention_days').value).toBe('60');
    });

    it('remembers the days the server had, across a Discard', async () => {
      const { element } = await open(sampleSettings({ trash_retention_days: 14 }));

      await flip(element, 'trash-auto-delete', false);
      await flip(element, 'trash-auto-delete', true);

      expect(field(element, 'trash_retention_days').value).toBe('14');
      expect(bar(element)).toBeNull();
    });

    it('lets the days be fixed by turning the deletion off', async () => {
      const { element } = await open(sampleSettings({ trash_retention_days: 14 }));

      await typeNumber(element, 'trash_retention_days', '');
      expect(save(element).disabled).toBe(true);
      await flip(element, 'trash-auto-delete', false);

      expect(save(element).disabled).toBe(false);
      expect(field(element, 'trash_retention_days').hasAttribute('aria-invalid')).toBe(false);
    });
  });

  describe('saving', () => {
    it('sends the whole document, unknown fields included, with the CSRF header', async () => {
      const unknown = { future_setting: { depth: 3 } };
      const doc = { ...sampleSettings(), ...unknown } as SettingsDto;
      const { element, requests } = await open(doc, ok(doc));
      await typeNumber(element, 'name_max_len', '143');

      await pressSave(element);
      await vi.waitFor(() => expect(requests).toHaveLength(2));

      const put = requests[1];
      expect(put).toMatchObject({ method: 'PUT', path: '/api/settings' });
      expect(put?.body).toEqual({ ...doc, name_max_len: 143 });
      expect(put?.headers.get(CSRF_HEADER)).toBe('1');
      expect(put?.headers.get('content-type')).toBe('application/json');
    });

    it('sends every edited field', async () => {
      const doc = sampleSettings();
      const { element, requests } = await open(doc, ok(doc));
      await addEntry(element, 'sidecar_extensions', 'raf');
      await removeEntry(element, 'junk_names', 'Thumbs.db');
      await addEntry(element, 'junk_prefixes', 'tmp_');
      await typeNumber(element, 'name_max_len', '143');
      await choose(element, 'rename');
      await flip(element, 'keep-cross-volume-source', true);
      await flip(element, 'trash-auto-delete', true);
      await typeNumber(element, 'trash_retention_days', '90');
      await typeNumber(element, 'index.concurrency', '4');
      await typeNumber(element, 'index.hash_concurrency', '2');
      await typeNumber(element, 'index.thumbnail_concurrency', '3');
      await flip(element, 'watch', false);
      await typeNumber(element, 'index.rescan_interval_minutes', '0');

      await pressSave(element);
      await vi.waitFor(() => expect(requests).toHaveLength(2));

      expect(requests[1]?.body).toEqual({
        ...doc,
        sidecar_extensions: [...doc.sidecar_extensions, 'raf'],
        junk_names: doc.junk_names.filter((name) => name !== 'Thumbs.db'),
        junk_prefixes: ['._', 'tmp_'],
        name_max_len: 143,
        default_conflict_policy: 'rename',
        keep_cross_volume_source_in_trash: true,
        trash_retention_days: 90,
        index: {
          concurrency: 4,
          hash_concurrency: 2,
          thumbnail_concurrency: 3,
          watch_enabled: false,
          rescan_interval_minutes: 0,
        },
      });
    });

    it('shows the document the server stored, and says so', async () => {
      const stored = sampleSettings({
        name_max_len: 200,
        sidecar_extensions: ['xmp', 'raf'],
        trash_retention_days: 7,
      });
      const { element } = await open(sampleSettings(), ok(stored));
      await addEntry(element, 'sidecar_extensions', 'RAF');
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(toastTexts()).toEqual(['success: Settings saved']));
      await element.updateComplete;

      expect(chips(element, 'sidecar_extensions').values).toEqual(['xmp', 'raf']);
      expect(field(element, 'name_max_len').value).toBe('200');
      expect(byId(element, 'trash-auto-delete').checked).toBe(true);
      expect(field(element, 'trash_retention_days').value).toBe('7');
      expect(bar(element)).toBeNull();
    });

    it('treats the stored document as the new baseline', async () => {
      const stored = sampleSettings({ name_max_len: 200 });
      const { element, requests } = await open(sampleSettings(), ok(stored), ok(sampleSettings()));
      await makeDirty(element);
      await pressSave(element);
      await vi.waitFor(() => expect(toastTexts()).toHaveLength(1));
      await element.updateComplete;

      await typeNumber(element, 'name_max_len', '255');
      expect(bar(element)).not.toBeNull();
      discard(element).click();
      await element.updateComplete;

      expect(field(element, 'name_max_len').value).toBe('200');
      expect(requests).toHaveLength(2);
    });

    it('freezes the form while it saves and releases it afterwards', async () => {
      const gate = deferred();
      const { element, requests } = await open(sampleSettings(), {
        ...ok(sampleSettings({ name_max_len: 200 })),
        gate: gate.promise,
      });
      await makeDirty(element);

      await pressSave(element);

      expect(requests).toHaveLength(2);
      for (const card of element.querySelectorAll('fieldset')) {
        expect(card.hasAttribute('disabled')).toBe(true);
      }
      expect(save(element).disabled).toBe(true);
      expect(save(element).querySelector('.loading')).not.toBeNull();
      expect(discard(element).disabled).toBe(true);
      expect(chips(element, 'junk_names').disabled).toBe(true);
      expect(field(element, 'name_max_len').disabled).toBe(true);
      expect(byId(element, 'watch').disabled).toBe(true);

      gate.resolve();
      await vi.waitFor(() => expect(toastTexts()).toHaveLength(1));
      await element.updateComplete;

      for (const card of element.querySelectorAll('fieldset')) {
        expect(card.hasAttribute('disabled')).toBe(false);
      }
      expect(chips(element, 'junk_names').disabled).toBe(false);
      expect(field(element, 'name_max_len').disabled).toBe(false);
    });

    it('sends one request even when Save is pressed twice', async () => {
      const { element, requests } = await open(sampleSettings(), ok(sampleSettings()));
      await makeDirty(element);
      const button = save(element);

      button.click();
      button.click();
      await vi.waitFor(() => expect(toastTexts()).toHaveLength(1));

      expect(requests).toHaveLength(2);
    });
  });

  describe('refused values', () => {
    it('shows the reason on a plain field, focuses it and clears it on the next edit', async () => {
      const { element } = await open(
        sampleSettings(),
        refused({ field: 'name_max_len', message: 'must be between 16 and 255' }),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() =>
        expect(field(element, 'name_max_len').hasAttribute('aria-invalid')).toBe(true),
      );

      expect(messageOf(element, 'name_max_len').textContent?.trim()).toBe(
        'Must be between 16 and 255',
      );
      expect(field(element, 'name_max_len').parentElement?.classList.contains('validator')).toBe(
        true,
      );
      await vi.waitFor(() => expect(document.activeElement).toBe(field(element, 'name_max_len')));
      expect(alertOf(element)).toBeNull();
      expect(toastTexts()).toEqual([]);
      expect(save(element).disabled).toBe(false);

      await typeNumber(element, 'name_max_len', '201');

      expect(field(element, 'name_max_len').hasAttribute('aria-invalid')).toBe(false);
    });

    it('shows the reason on a nested field', async () => {
      const { element } = await open(
        sampleSettings(),
        refused({ field: 'index.concurrency', message: 'must be between 1 and 16' }),
      );
      await typeNumber(element, 'index.concurrency', '4');

      await pressSave(element);
      await vi.waitFor(() =>
        expect(field(element, 'index.concurrency').getAttribute('aria-invalid')).toBe('true'),
      );

      expect(messageOf(element, 'index.concurrency').textContent?.trim()).toBe(
        'Must be between 1 and 16',
      );
      await vi.waitFor(() =>
        expect(document.activeElement).toBe(field(element, 'index.concurrency')),
      );
      expect(field(element, 'index.hash_concurrency').hasAttribute('aria-invalid')).toBe(false);
    });

    it('focuses the first of several refused fields, in page order', async () => {
      const { element } = await open(
        sampleSettings(),
        refused(
          {
            field: 'index.rescan_interval_minutes',
            message: 'must be 0 (never) or between 5 and 10080',
          },
          { field: 'index.concurrency', message: 'must be between 1 and 16' },
          { field: 'name_max_len', message: 'must be between 16 and 255' },
        ),
      );
      await makeDirty(element);

      await pressSave(element);

      await vi.waitFor(() => expect(document.activeElement).toBe(field(element, 'name_max_len')));
      for (const name of ['name_max_len', 'index.concurrency', 'index.rescan_interval_minutes']) {
        expect(field(element, name).getAttribute('aria-invalid'), name).toBe('true');
      }
    });

    it('marks the chip of a refused entry and quotes it', async () => {
      const { element } = await open(
        sampleSettings(),
        refused({
          field: 'sidecar_extensions[1]',
          message: 'must be 1 to 10 lowercase letters or digits, without a dot',
        }),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() =>
        expect(chips(element, 'sidecar_extensions').errorIndexes).toEqual([1]),
      );
      await chips(element, 'sidecar_extensions').updateComplete;

      const list = chips(element, 'sidecar_extensions');
      expect(list.error).toBe('"json" must be 1 to 10 lowercase letters or digits, without a dot');
      expect(
        [...list.querySelectorAll('li')].map((chip) => chip.classList.contains('badge-error')),
      ).toEqual([false, true, false, false, false, false]);
      expect(list.querySelector('p')?.textContent).toContain('"json" must be 1 to 10');
      expect(list.querySelector('input')?.getAttribute('aria-invalid')).toBe('true');
      await vi.waitFor(() => expect(document.activeElement).toBe(list.querySelector('input')));
      expect(chips(element, 'junk_names').errorIndexes).toEqual([]);
    });

    it('sums up several refused entries of one list', async () => {
      const { element } = await open(
        sampleSettings(),
        refused(
          { field: 'junk_names[0]', message: 'must not contain `/` or NUL' },
          { field: 'junk_names[2]', message: 'is too long (at most 255 bytes)' },
          { field: 'junk_names[3]', message: 'must not be `.` or `..`' },
          { field: 'junk_names[4]', message: 'must not be `.` or `..`' },
        ),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() =>
        expect(chips(element, 'junk_names').errorIndexes).toEqual([0, 2, 3, 4]),
      );

      expect(chips(element, 'junk_names').error).toBe(
        '"@eaDir" must not contain `/` or NUL; "#snapshot" is too long (at most 255 bytes); ".DS_Store" must not be `.` or `..`; and 1 more',
      );
    });

    it('shows a problem of a whole list under the list', async () => {
      const { element } = await open(
        sampleSettings(),
        refused({ field: 'sidecar_extensions', message: 'at most 50 extensions are allowed' }),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(chips(element, 'sidecar_extensions').error).not.toBe(''));

      expect(chips(element, 'sidecar_extensions').error).toBe('At most 50 extensions are allowed');
      expect(chips(element, 'sidecar_extensions').errorIndexes).toEqual([]);
    });

    it('clears the marks of a list when it is edited', async () => {
      const { element } = await open(
        sampleSettings(),
        refused({ field: 'sidecar_extensions[1]', message: 'must be 1 to 10 lowercase letters' }),
      );
      await makeDirty(element);
      await pressSave(element);
      await vi.waitFor(() =>
        expect(chips(element, 'sidecar_extensions').errorIndexes).toEqual([1]),
      );

      await removeEntry(element, 'sidecar_extensions', 'json');

      expect(chips(element, 'sidecar_extensions').errorIndexes).toEqual([]);
      expect(chips(element, 'sidecar_extensions').error).toBe('');
    });

    it('lists what no field can show in an alert above the form', async () => {
      const { element } = await open(
        sampleSettings(),
        refused(
          { field: 'default_conflict_policy', message: 'is not supported' },
          { field: 'index.future_knob', message: 'must be even' },
          { field: 'junk_names[99]x', message: 'odd' },
        ),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(alertOf(element)).not.toBeNull());

      const lines = [...(alertOf(element)?.querySelectorAll('li') ?? [])].map(
        (line) => line.textContent,
      );
      expect(lines).toEqual([
        'default_conflict_policy: is not supported',
        'index.future_knob: must be even',
        'junk_names[99]x: odd',
      ]);
      expect(alertOf(element)?.textContent).toContain('The settings were not saved');
      expect(element.querySelector('[aria-invalid="true"]')).toBeNull();
      expect(element.querySelector('form')?.firstElementChild).toBe(alertOf(element));
    });

    it('shows both a field and an alert when the answer has both', async () => {
      const { element } = await open(
        sampleSettings(),
        refused(
          { field: 'name_max_len', message: 'must be between 16 and 255' },
          { field: 'surprise', message: 'is unknown' },
        ),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(alertOf(element)).not.toBeNull());

      expect(alertOf(element)?.textContent).toContain('surprise: is unknown');
      expect(field(element, 'name_max_len').getAttribute('aria-invalid')).toBe('true');
    });

    it('shows the message of a validation error without fields in the alert', async () => {
      const { element } = await open(
        sampleSettings(),
        problem(422, 'validation_failed', 'the settings are not acceptable'),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(alertOf(element)).not.toBeNull());

      expect(alertOf(element)?.querySelectorAll('li')).toHaveLength(1);
      expect(alertOf(element)?.textContent).toContain('the settings are not acceptable');
      expect(toastTexts()).toEqual([]);
    });

    it.each([400, 415, 422])(
      'shows an invalid_json answer (status %i) in the alert',
      async (status) => {
        const { element } = await open(
          sampleSettings(),
          problem(status, 'invalid_json', 'missing field `index`'),
        );
        await makeDirty(element);

        await pressSave(element);
        await vi.waitFor(() => expect(alertOf(element)).not.toBeNull());

        expect(alertOf(element)?.textContent).toContain('missing field `index`');
        expect(toastTexts()).toEqual([]);
      },
    );

    it('clears the messages when the next save starts', async () => {
      const gate = deferred();
      const { element } = await open(
        sampleSettings(),
        refused({ field: 'name_max_len', message: 'must be between 16 and 255' }),
        { ...ok(sampleSettings({ name_max_len: 201 })), gate: gate.promise },
      );
      await makeDirty(element);
      await pressSave(element);
      await vi.waitFor(() =>
        expect(field(element, 'name_max_len').hasAttribute('aria-invalid')).toBe(true),
      );
      await typeNumber(element, 'name_max_len', '201');

      await pressSave(element);

      expect(element.querySelector('[aria-invalid="true"]')).toBeNull();
      gate.resolve();
      await vi.waitFor(() => expect(toastTexts()).toEqual(['success: Settings saved']));
    });

    it('clears the messages on Discard', async () => {
      const { element } = await open(sampleSettings(), problem(422, 'invalid_json', 'a bug'));
      await makeDirty(element);
      await pressSave(element);
      await vi.waitFor(() => expect(alertOf(element)).not.toBeNull());

      discard(element).click();
      await element.updateComplete;

      expect(alertOf(element)).toBeNull();
    });

    it('keeps the edits after a refusal', async () => {
      const { element } = await open(
        sampleSettings(),
        refused({ field: 'name_max_len', message: 'must be between 16 and 255' }),
      );
      await addEntry(element, 'junk_names', 'Backup');
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() =>
        expect(field(element, 'name_max_len').hasAttribute('aria-invalid')).toBe(true),
      );

      expect(field(element, 'name_max_len').value).toBe('200');
      expect(chips(element, 'junk_names').values).toContain('Backup');
      expect(bar(element)).not.toBeNull();
    });
  });

  describe('other failures', () => {
    it('shows a toast and keeps the form when the database is busy', async () => {
      const { element } = await open(
        sampleSettings(),
        problem(503, 'db_busy', 'The database is busy, try again'),
      );
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(toasts.value).toHaveLength(1));

      expect(toastTexts()).toEqual(['error: The database is busy, try again']);
      expect(alertOf(element)).toBeNull();
      expect(field(element, 'name_max_len').value).toBe('200');
      expect(bar(element)).not.toBeNull();
      expect(save(element).disabled).toBe(false);
      expect(element.querySelector('fieldset')?.hasAttribute('disabled')).toBe(false);
    });

    it('shows a toast when the server cannot be reached, and saves again on request', async () => {
      const { element, requests } = await open(
        sampleSettings(),
        ok(sampleSettings({ name_max_len: 200 })),
      );
      await makeDirty(element);
      const failing = vi.fn(async () => {
        throw new TypeError('Failed to fetch');
      });
      vi.stubGlobal('fetch', failing);

      await pressSave(element);
      await vi.waitFor(() => expect(toasts.value).toHaveLength(1));

      expect(toasts.value[0]?.kind).toBe('error');
      expect(toasts.value[0]?.message).toContain('Could not reach the server');
      expect(bar(element)).not.toBeNull();

      serve(ok(sampleSettings({ name_max_len: 200 })));
      await pressSave(element);
      await vi.waitFor(() => expect(bar(element)).toBeNull());
      expect(requests).toHaveLength(1);
    });
  });

  describe('leaving the page', () => {
    it('does not ask while nothing changed', async () => {
      await open();

      expect(leave()).toBe(false);
    });

    it('asks while there are unsaved changes', async () => {
      const { element } = await open();

      await makeDirty(element);

      expect(leave()).toBe(true);
    });

    it('stops asking once the change is undone, discarded or saved', async () => {
      const { element } = await open(sampleSettings(), ok(sampleSettings({ name_max_len: 200 })));

      await makeDirty(element);
      await typeNumber(element, 'name_max_len', '255');
      expect(leave()).toBe(false);

      await makeDirty(element);
      discard(element).click();
      await element.updateComplete;
      expect(leave()).toBe(false);

      await makeDirty(element);
      expect(leave()).toBe(true);
      await pressSave(element);
      await vi.waitFor(() => expect(toastTexts()).toHaveLength(1));
      await element.updateComplete;
      expect(leave()).toBe(false);
    });

    it('keeps asking after a refused save', async () => {
      const { element } = await open(sampleSettings(), problem(503, 'db_busy', 'busy'));
      await makeDirty(element);

      await pressSave(element);
      await vi.waitFor(() => expect(toasts.value).toHaveLength(1));

      expect(leave()).toBe(true);
    });

    it('stops listening when the form leaves the page', async () => {
      const { element } = await open();
      await makeDirty(element);
      expect(leave()).toBe(true);

      element.remove();

      expect(leave()).toBe(false);
    });

    it('listens again when a form with changes comes back', async () => {
      const { element } = await open();
      await makeDirty(element);
      const parent = element.parentElement as HTMLElement;
      element.remove();

      parent.append(element);
      await element.updateComplete;

      expect(leave()).toBe(true);
    });
  });
});
