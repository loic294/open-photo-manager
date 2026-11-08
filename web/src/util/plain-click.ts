/**
 * True for a click the page handles itself: the primary button, no modifier key, not claimed by
 * another handler. A middle click, Cmd/Ctrl/Shift/Alt + click ("open in a new tab") and the like
 * are left to the browser, which follows the link's real `href`.
 */
export function isPlainClick(event: MouseEvent): boolean {
  return (
    !event.defaultPrevented &&
    event.button === 0 &&
    !event.metaKey &&
    !event.ctrlKey &&
    !event.shiftKey &&
    !event.altKey
  );
}
