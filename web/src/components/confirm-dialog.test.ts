import { html } from 'lit';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ApmModal } from '@/components/apm-modal';
import { confirmDialog } from '@/components/confirm-dialog';

function hosts() {
  return document.querySelectorAll('apm-confirm-dialog');
}

function modal(): ApmModal {
  const found = document.querySelector('apm-confirm-dialog apm-modal');
  if (!found) throw new Error('the confirm dialog is not mounted');
  return found as ApmModal;
}

async function shown(): Promise<ApmModal> {
  await vi.waitFor(() => {
    expect(document.querySelector('apm-confirm-dialog apm-modal')).not.toBeNull();
    expect(modal().open).toBe(true);
  });
  await modal().updateComplete;
  return modal();
}

function button(label: string): HTMLButtonElement {
  const found = [...document.querySelectorAll('apm-confirm-dialog .modal-action button')].find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!found) throw new Error(`no "${label}" button`);
  return found as HTMLButtonElement;
}

async function closed(): Promise<void> {
  await vi.waitFor(() => expect(modal().open).toBe(false));
}

afterEach(() => {
  for (const host of hosts()) host.remove();
});

describe('confirmDialog', () => {
  it('mounts one shared dialog lazily and reuses it', async () => {
    expect(hosts()).toHaveLength(0);

    const first = confirmDialog({ title: 'First', message: 'one' });
    await shown();
    button('Cancel').click();
    await first;
    const second = confirmDialog({ title: 'Second', message: 'two' });
    await shown();
    button('Cancel').click();
    await second;

    expect(hosts()).toHaveLength(1);
  });

  it('shows the title, message and default labels in a modal dialog', async () => {
    const answer = confirmDialog({ title: 'Delete 3 files?', message: 'They go to the trash.' });
    const dialog = await shown();

    expect(dialog.querySelector('h3')?.textContent).toBe('Delete 3 files?');
    expect(dialog.querySelector('.modal-box')?.textContent).toContain('They go to the trash.');
    expect(button('Confirm')).toBeDefined();
    expect(button('Cancel')).toBeDefined();
    expect(dialog.querySelector('dialog')?.open).toBe(true);

    button('Cancel').click();
    await answer;
  });

  it('resolves true when confirmed and closes', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'sure' });
    await shown();

    button('Confirm').click();

    await expect(answer).resolves.toBe(true);
    await closed();
  });

  it('resolves false when cancelled and closes', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'sure' });
    await shown();

    button('Cancel').click();

    await expect(answer).resolves.toBe(false);
    await closed();
  });

  it('uses custom labels', async () => {
    const answer = confirmDialog({
      title: 'Move?',
      message: 'm',
      confirmLabel: 'Move 4 files',
      cancelLabel: 'Keep them',
    });
    await shown();

    expect(button('Move 4 files')).toBeDefined();
    button('Keep them').click();
    await answer;
  });

  it('resolves false on Escape', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'sure' });
    const dialog = await shown();

    dialog.querySelector('dialog')?.dispatchEvent(new Event('cancel', { cancelable: true }));

    await expect(answer).resolves.toBe(false);
    await closed();
  });

  it('resolves false on a click outside', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'sure' });
    const dialog = await shown();

    dialog.querySelector<HTMLButtonElement>('.modal-backdrop button')?.click();

    await expect(answer).resolves.toBe(false);
  });

  it('resolves false with the X button', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'sure' });
    const dialog = await shown();

    dialog.querySelector<HTMLButtonElement>('button[aria-label="Close"]')?.click();

    await expect(answer).resolves.toBe(false);
  });

  it('starts focus on the confirm button of a normal dialog', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'sure' });
    await shown();

    await vi.waitFor(() => expect(document.activeElement).toBe(button('Confirm')));
    expect(button('Confirm').classList.contains('btn-primary')).toBe(true);
    expect(button('Cancel').hasAttribute('autofocus')).toBe(false);

    button('Cancel').click();
    await answer;
  });

  it('makes a dangerous dialog red, warns, and starts focus on Cancel', async () => {
    const answer = confirmDialog({
      title: 'Delete?',
      message: 'This cannot be undone.',
      confirmLabel: 'Delete',
      danger: true,
    });
    const dialog = await shown();

    expect(button('Delete').classList.contains('btn-error')).toBe(true);
    expect(dialog.querySelector('.alert')?.textContent).toContain('This cannot be undone.');
    await vi.waitFor(() => expect(document.activeElement).toBe(button('Cancel')));
    expect(button('Delete').hasAttribute('autofocus')).toBe(false);

    button('Cancel').click();
    await answer;
  });

  it('lets screen readers read the message with the dialog', async () => {
    const answer = confirmDialog({ title: 'Go?', message: 'read me' });
    const dialog = await shown();

    const describedBy = dialog.querySelector('dialog')?.getAttribute('aria-describedby');
    expect(describedBy).toBeTruthy();
    expect(document.getElementById(describedBy ?? '')?.textContent).toContain('read me');

    button('Cancel').click();
    await answer;
  });

  it('renders a template as the message', async () => {
    const answer = confirmDialog({
      title: 'Delete?',
      message: html`<strong id="count">3 files</strong> (12 MB)`,
    });
    const dialog = await shown();

    expect(dialog.querySelector('#count')?.textContent).toBe('3 files');

    button('Cancel').click();
    await answer;
  });

  it('shows plain-text messages as text, never as markup', async () => {
    const answer = confirmDialog({ title: 'x', message: '<img src=x onerror=alert(1)>' });
    const dialog = await shown();

    expect(dialog.querySelector('.modal-box img')).toBeNull();

    button('Cancel').click();
    await answer;
  });

  it('answers calls one after another, first come first served', async () => {
    const order: string[] = [];
    const first = confirmDialog({ title: 'First', message: 'a' }).then((value) => {
      order.push(`first:${value}`);
    });
    const second = confirmDialog({ title: 'Second', message: 'b', danger: true }).then((value) => {
      order.push(`second:${value}`);
    });

    const dialog = await shown();
    expect(dialog.querySelector('h3')?.textContent).toBe('First');

    button('Confirm').click();
    await first;

    await vi.waitFor(() => expect(modal().querySelector('h3')?.textContent).toBe('Second'));
    expect(modal().open).toBe(true);
    expect(order).toEqual(['first:true']);

    await vi.waitFor(() => expect(document.activeElement).toBe(button('Cancel')));
    button('Cancel').click();
    await second;

    expect(order).toEqual(['first:true', 'second:false']);
  });

  it('shows a queued dialog after the previous one was dismissed with Escape', async () => {
    const first = confirmDialog({ title: 'First', message: 'a' });
    const second = confirmDialog({ title: 'Second', message: 'b' });
    const dialog = await shown();

    dialog.querySelector('dialog')?.dispatchEvent(new Event('cancel', { cancelable: true }));
    await expect(first).resolves.toBe(false);

    await vi.waitFor(() => expect(modal().querySelector('h3')?.textContent).toBe('Second'));
    await vi.waitFor(() => expect(modal().open).toBe(true));
    expect(modal().querySelector('dialog')?.open).toBe(true);

    button('Confirm').click();
    await expect(second).resolves.toBe(true);
  });

  it('mounts a fresh dialog if the shared one was removed from the page', async () => {
    const first = confirmDialog({ title: 'One', message: 'a' });
    await shown();
    button('Cancel').click();
    await first;
    for (const host of hosts()) host.remove();

    const second = confirmDialog({ title: 'Two', message: 'b' });
    const dialog = await shown();

    expect(hosts()).toHaveLength(1);
    expect(dialog.querySelector('h3')?.textContent).toBe('Two');
    button('Cancel').click();
    await second;
  });

  it('answers everything still waiting with false when the dialog is removed', async () => {
    const first = confirmDialog({ title: 'One', message: 'a' });
    const second = confirmDialog({ title: 'Two', message: 'b' });
    await shown();

    for (const host of hosts()) host.remove();

    await expect(first).resolves.toBe(false);
    await expect(second).resolves.toBe(false);
  });
});

describe('confirmDialog asked again right after an answer', () => {
  async function settleQuietly(): Promise<void> {
    await new Promise((resolve) => window.setTimeout(resolve, 30));
  }

  it('keeps the next dialog open when the previous one is still closing', async () => {
    const first = confirmDialog({ title: 'First', message: 'a' });
    await shown();
    let answeredEarly: boolean | undefined;
    let second: Promise<boolean> | undefined;
    // Asks exactly when the modal reports that it closed for the first request, before the
    // dialog got to see that event.
    document.addEventListener(
      'close',
      () => {
        second = confirmDialog({ title: 'Second', message: 'b' });
        void second.then((value) => {
          answeredEarly = value;
        });
      },
      { capture: true, once: true },
    );

    button('Confirm').click();
    await expect(first).resolves.toBe(true);
    await vi.waitFor(() => expect(second).toBeDefined());
    await vi.waitFor(() => expect(modal().querySelector('h3')?.textContent).toBe('Second'));
    await settleQuietly();

    expect(answeredEarly).toBeUndefined();
    expect(modal().open).toBe(true);
    expect(modal().querySelector('dialog')?.open).toBe(true);

    button('Confirm').click();
    await expect(second).resolves.toBe(true);
  });

  it('works from the continuation of the previous answer', async () => {
    let answeredEarly: boolean | undefined;
    const first = confirmDialog({ title: 'First', message: 'a' });
    const second = first.then(() =>
      confirmDialog({ title: 'Second', message: 'b' }).then((value) => {
        answeredEarly = value;
        return value;
      }),
    );
    await shown();

    button('Confirm').click();
    await vi.waitFor(() => expect(modal().querySelector('h3')?.textContent).toBe('Second'));
    await settleQuietly();

    expect(answeredEarly).toBeUndefined();
    expect(modal().querySelector('dialog')?.open).toBe(true);

    button('Cancel').click();
    await expect(second).resolves.toBe(false);
  });
});
