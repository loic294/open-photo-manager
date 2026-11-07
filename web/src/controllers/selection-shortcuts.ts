import { activePane, type PaneId, selection } from '@/stores/selection';

/** Inputs that take no typed text: Cmd+A and Escape mean nothing to them, so the app may act. */
const NON_TEXT_INPUT_TYPES: ReadonlySet<string> = new Set([
  'button',
  'checkbox',
  'color',
  'file',
  'hidden',
  'image',
  'radio',
  'range',
  'reset',
  'submit',
]);

const EDITABLE_ANCESTOR = '[contenteditable]:not([contenteditable="false"]), [role="textbox"]';

/**
 * True when the key press belongs to the focused control: a text field, a `textarea`, a `select`,
 * or editable content. Checkboxes, sliders and buttons do not count: after a click on a card's
 * checkbox or on the size slider they keep the focus, and Cmd+A must still select the items.
 */
function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target instanceof HTMLInputElement) return !NON_TEXT_INPUT_TYPES.has(target.type);
  if (target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return true;
  return target.closest(EDITABLE_ANCESTOR) !== null;
}

function hasOpenDialog(target: Document | HTMLElement): boolean {
  return (target.ownerDocument ?? target).querySelector('dialog[open]') !== null;
}

function isSelectAll(event: KeyboardEvent): boolean {
  return (
    (event.metaKey || event.ctrlKey) &&
    !event.shiftKey &&
    !event.altKey &&
    event.key.toLowerCase() === 'a'
  );
}

function isPlainEscape(event: KeyboardEvent): boolean {
  return (
    event.key === 'Escape' && !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey
  );
}

/**
 * Clears the active pane; when it has nothing to clear (nothing selected, nothing loading) clears
 * the others, so a second Escape empties everything.
 */
function clearForEscape(): void {
  const current = selection[activePane.peek()];
  if (current.count.peek() > 0 || current.busy.peek()) {
    current.clear();
    return;
  }
  for (const pane of Object.keys(selection) as PaneId[]) {
    if (selection[pane] !== current) selection[pane].clear();
  }
}

/**
 * Installs the selection shortcuts and returns the function that removes them. The tool view calls
 * it once in `connectedCallback`.
 * - Cmd/Ctrl+A selects every item of the ACTIVE pane (`activePane`, the one touched last), and
 *   stops the browser from selecting the page text.
 * - Escape clears the active pane's selection, then the other pane's.
 *
 * Both stand aside while a text field has the focus, once something has handled the key
 * (`defaultPrevented`) and while a modal `<dialog>` is open. Arrows, Space and Enter belong to the
 * grid, Cmd+click and Shift+click to the card.
 */
export function installSelectionShortcuts(target: Document | HTMLElement = document): () => void {
  const listener = (event: Event): void => {
    if (!(event instanceof KeyboardEvent) || event.defaultPrevented) return;
    const selectAll = isSelectAll(event);
    if (!selectAll && !isPlainEscape(event)) return;
    if (hasOpenDialog(target) || isEditable(event.composedPath()[0] ?? event.target)) return;
    if (selectAll) {
      event.preventDefault();
      void selection[activePane.peek()].selectAll();
    } else {
      clearForEscape();
    }
  };
  const events: EventTarget = target;
  events.addEventListener('keydown', listener);
  return () => events.removeEventListener('keydown', listener);
}
