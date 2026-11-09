import { html } from 'lit';
import { describe, expect, it, vi } from 'vitest';
import '@/components/apm-modal';
import { mount } from '@/test/helpers';

function dialogOf(modal: HTMLElement): HTMLDialogElement {
  const dialog = modal.querySelector('dialog');
  if (!dialog) throw new Error('no dialog rendered');
  return dialog;
}

function listen(modal: HTMLElement) {
  const closed = vi.fn();
  modal.addEventListener('close', closed);
  return closed;
}

describe('apm-modal: markup', () => {
  it('renders a closed DaisyUI modal', async () => {
    const modal = await mount('apm-modal', { heading: 'Rename' });

    const dialog = dialogOf(modal);
    expect(dialog.classList.contains('modal')).toBe(true);
    expect(dialog.open).toBe(false);
    expect(modal.querySelector('.modal-box')).not.toBeNull();
    expect(modal.querySelector('.modal-box')?.classList.contains('relative')).toBe(true);
  });

  it('labels the dialog with its heading', async () => {
    const modal = await mount('apm-modal', { heading: 'Rename folder' });

    const dialog = dialogOf(modal);
    const heading = modal.querySelector('h3');
    expect(heading?.textContent).toBe('Rename folder');
    expect(dialog.getAttribute('aria-labelledby')).toBe(heading?.id);
    expect(heading?.id).not.toBe('');
  });

  it('gives each modal its own heading id', async () => {
    const first = await mount('apm-modal', { heading: 'One' });
    const second = await mount('apm-modal', { heading: 'Two' });

    expect(first.querySelector('h3')?.id).not.toBe(second.querySelector('h3')?.id);
  });

  it('has no label reference without a heading', async () => {
    const modal = await mount('apm-modal');

    expect(dialogOf(modal).hasAttribute('aria-labelledby')).toBe(false);
  });

  it('renders the body and the actions from templates', async () => {
    const modal = await mount('apm-modal', {
      heading: 'Delete',
      body: html`<p id="msg">Sure?</p>`,
      actions: html`<button class="btn" id="go">Go</button>`,
    });

    expect(modal.querySelector('.modal-box #msg')?.textContent).toBe('Sure?');
    expect(modal.querySelector('.modal-box .modal-action #go')).not.toBeNull();
  });

  it('accepts plain text as the body', async () => {
    const modal = await mount('apm-modal', { body: 'Plain <b>text</b>' });

    expect(modal.querySelector('.modal-box')?.textContent).toContain('Plain <b>text</b>');
    expect(modal.querySelector('.modal-box b')).toBeNull();
  });

  it('omits the action row when there are no actions', async () => {
    const modal = await mount('apm-modal', { body: 'x' });

    expect(modal.querySelector('.modal-action')).toBeNull();
  });

  it.each([
    ['sm', 'max-w-sm'],
    ['md', 'max-w-lg'],
    ['lg', 'max-w-3xl'],
    ['xl', 'max-w-5xl'],
    ['full', 'max-w-none'],
  ] as const)('maps size %s to %s on the modal box', async (size, expected) => {
    const modal = await mount('apm-modal', { size });

    expect(modal.querySelector('.modal-box')?.classList.contains(expected)).toBe(true);
  });

  it('defaults to the medium size', async () => {
    const modal = await mount('apm-modal');

    expect(modal.size).toBe('md');
  });

  it('makes the full size a column whose body fills what the heading leaves', async () => {
    const modal = await mount('apm-modal', {
      size: 'full',
      heading: 'A very long file name.jpg',
      body: html`<p id="msg">x</p>`,
    });

    const box = modal.querySelector('.modal-box');
    for (const name of ['flex', 'flex-col', 'h-full', 'w-full', 'max-w-none']) {
      expect(box?.classList.contains(name), name).toBe(true);
    }
    const body = modal.querySelector('#msg')?.parentElement;
    for (const name of ['flex', 'flex-col', 'flex-1', 'min-h-0']) {
      expect(body?.classList.contains(name), name).toBe(true);
    }
    expect(body?.classList.contains('py-4')).toBe(false);
    const heading = modal.querySelector('h3');
    expect(heading?.classList.contains('truncate')).toBe(true);
    expect(heading?.classList.contains('min-w-0')).toBe(true);
    expect(heading?.textContent).toBe('A very long file name.jpg');
  });

  it.each(['sm', 'md', 'lg', 'xl'] as const)(
    'keeps the %s size a plain box with a padded body and a wrapping heading',
    async (size) => {
      const modal = await mount('apm-modal', {
        size,
        heading: 'Title',
        body: html`<p id="msg">x</p>`,
      });

      const box = modal.querySelector('.modal-box');
      expect(box?.classList.contains('flex')).toBe(false);
      expect(modal.querySelector('#msg')?.parentElement?.className).toBe('py-4');
      expect(modal.querySelector('h3')?.classList.contains('truncate')).toBe(false);
    },
  );

  it('points screen readers at the described element', async () => {
    const modal = await mount('apm-modal', { describedBy: 'msg', body: html`<p id="msg">x</p>` });

    expect(dialogOf(modal).getAttribute('aria-describedby')).toBe('msg');
  });
});

describe('apm-modal: opening and closing', () => {
  it('shows the native dialog modally when open is set, and reflects the attribute', async () => {
    const modal = await mount('apm-modal', { heading: 'x' });
    const showModal = vi.spyOn(dialogOf(modal), 'showModal');

    modal.open = true;
    await modal.updateComplete;

    expect(showModal).toHaveBeenCalledTimes(1);
    expect(dialogOf(modal).open).toBe(true);
    expect(modal.hasAttribute('open')).toBe(true);
  });

  it('opens straight away when created open', async () => {
    const modal = await mount('apm-modal', { open: true });

    expect(dialogOf(modal).open).toBe(true);
  });

  it('closes the dialog when open is cleared and fires close once', async () => {
    const modal = await mount('apm-modal', { open: true });
    const closed = listen(modal);

    modal.open = false;
    await modal.updateComplete;

    expect(dialogOf(modal).open).toBe(false);
    expect(modal.hasAttribute('open')).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
  });

  it('does not fire close for a modal that was never opened', async () => {
    const modal = await mount('apm-modal');
    const closed = listen(modal);

    modal.heading = 'changed';
    await modal.updateComplete;

    expect(closed).not.toHaveBeenCalled();
  });

  it('can be driven imperatively with show() and close()', async () => {
    const modal = await mount('apm-modal');
    const closed = listen(modal);

    await modal.show();
    expect(dialogOf(modal).open).toBe(true);
    expect(modal.open).toBe(true);

    await modal.close();
    expect(dialogOf(modal).open).toBe(false);
    expect(modal.open).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
  });

  it('can be reopened after closing', async () => {
    const modal = await mount('apm-modal');

    await modal.show();
    await modal.close();
    await modal.show();

    expect(dialogOf(modal).open).toBe(true);
  });
});

describe('apm-modal: dismissing', () => {
  it('closes on Escape (the cancel event) and takes over from the browser', async () => {
    const modal = await mount('apm-modal', { open: true });
    const closed = listen(modal);

    const cancel = new Event('cancel', { cancelable: true });
    dialogOf(modal).dispatchEvent(cancel);
    await modal.updateComplete;

    expect(cancel.defaultPrevented).toBe(true);
    expect(modal.open).toBe(false);
    expect(dialogOf(modal).open).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
  });

  it('closes when the backdrop is clicked, without submitting the form', async () => {
    const modal = await mount('apm-modal', { open: true });
    const closed = listen(modal);
    const button = modal.querySelector<HTMLButtonElement>('.modal-backdrop button');

    const click = new MouseEvent('click', { bubbles: true, cancelable: true });
    button?.dispatchEvent(click);
    await modal.updateComplete;

    expect(click.defaultPrevented).toBe(true);
    expect(modal.open).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
  });

  it('keeps the invisible backdrop button out of the tab order and the accessibility tree', async () => {
    const modal = await mount('apm-modal');

    const button = modal.querySelector('.modal-backdrop button');
    expect(button?.getAttribute('tabindex')).toBe('-1');
    expect(button?.getAttribute('aria-hidden')).toBe('true');
  });

  it('does not place transparent backdrop text over the modal content', async () => {
    const modal = await mount('apm-modal', { open: true });

    expect(modal.querySelector('.modal-backdrop button')?.textContent?.trim()).toBe('');
  });

  it('closes from the X button, which has an accessible name', async () => {
    const modal = await mount('apm-modal', { open: true, heading: 'x' });
    const closed = listen(modal);
    const button = modal.querySelector<HTMLButtonElement>('button[aria-label="Close"]');

    button?.click();
    await modal.updateComplete;

    expect(modal.open).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
  });

  it('shows the X button even without a heading', async () => {
    const modal = await mount('apm-modal');

    expect(modal.querySelector('button[aria-label="Close"]')).not.toBeNull();
  });

  it('stays open on Escape, backdrop and X when persistent, and offers none of them', async () => {
    const modal = await mount('apm-modal', { open: true, heading: 'Busy', persistent: true });
    const closed = listen(modal);

    const cancel = new Event('cancel', { cancelable: true });
    dialogOf(modal).dispatchEvent(cancel);
    await modal.updateComplete;

    expect(cancel.defaultPrevented).toBe(true);
    expect(modal.open).toBe(true);
    expect(dialogOf(modal).open).toBe(true);
    expect(closed).not.toHaveBeenCalled();
    expect(modal.querySelector('button[aria-label="Close"]')).toBeNull();
    expect(modal.querySelector('.modal-backdrop')).toBeNull();
  });

  it('can still be closed by code when persistent', async () => {
    const modal = await mount('apm-modal', { open: true, persistent: true });

    await modal.close();

    expect(dialogOf(modal).open).toBe(false);
  });

  it('follows a close that the browser made itself, and fires close once', async () => {
    const modal = await mount('apm-modal', { open: true });
    const closed = listen(modal);

    dialogOf(modal).close();
    dialogOf(modal).dispatchEvent(new Event('close'));
    await modal.updateComplete;

    expect(modal.open).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
  });

  it('ignores a late native close event after it was opened again', async () => {
    const modal = await mount('apm-modal', { open: true });

    await modal.close();
    await modal.show();
    dialogOf(modal).dispatchEvent(new Event('close'));
    await modal.updateComplete;

    expect(modal.open).toBe(true);
    expect(dialogOf(modal).open).toBe(true);
  });
});

describe('apm-modal: focus', () => {
  it('returns focus to the element that had it before opening', async () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    const modal = await mount('apm-modal');

    await modal.show();
    (modal.querySelector('button[aria-label="Close"]') as HTMLButtonElement).focus();
    await modal.close();

    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it('does not steal focus back when it already moved elsewhere', async () => {
    const opener = document.createElement('button');
    const other = document.createElement('input');
    document.body.append(opener, other);
    opener.focus();
    const modal = await mount('apm-modal');

    await modal.show();
    other.focus();
    await modal.close();

    expect(document.activeElement).toBe(other);
    opener.remove();
    other.remove();
  });
});
