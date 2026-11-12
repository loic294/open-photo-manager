import { afterEach, expect, vi } from 'vitest';
import type { OpmModal } from '@/components/opm-modal';

const HOST = 'opm-confirm-dialog';

// The shared dialog outlives a test; removing it also answers anything still waiting with false.
afterEach(() => {
  for (const host of document.querySelectorAll(HOST)) host.remove();
});

const modal = () => document.querySelector<OpmModal>(`${HOST} opm-modal`);

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

/** What a user reads in the confirm dialog. */
export interface ShownConfirm {
  title: string;
  message: string;
  /** The labels of the buttons, Cancel first. */
  buttons: string[];
}

/** Waits until `confirmDialog()` shows its dialog and reports what it says. */
export async function shownConfirm(): Promise<ShownConfirm> {
  await vi.waitFor(() => expect(modal()?.open).toBe(true));
  await modal()?.updateComplete;
  return {
    title: text(modal()?.querySelector('h3')),
    message: text(modal()?.querySelector('#opm-confirm-message')),
    buttons: [...(modal()?.querySelectorAll('.modal-action button') ?? [])].map(text),
  };
}

/** Waits for the confirm dialog, then clicks its button with this label (`Cancel` or the confirm label). */
export async function pressConfirm(label: string): Promise<void> {
  await shownConfirm();
  const button = [
    ...document.querySelectorAll<HTMLButtonElement>(`${HOST} .modal-action button`),
  ].find((candidate) => text(candidate) === label);
  if (!button) throw new Error(`The confirm dialog has no "${label}" button`);
  button.click();
}
