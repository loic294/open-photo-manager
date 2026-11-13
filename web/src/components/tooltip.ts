import { noChange } from 'lit';
import { AsyncDirective } from 'lit/async-directive.js';
import { directive, type ElementPart, type PartInfo, PartType } from 'lit/directive.js';

let nextId = 0;
let dismissActive: (() => void) | undefined;

/** DaisyUI help in the top layer, so scrolling panes and modal dialogs cannot clip it. */
class TooltipDirective extends AsyncDirective {
  private element: Element | undefined;
  private text = '';
  private popup: HTMLDivElement | undefined;
  private hovered = false;
  private focused = false;
  private hideTimer: number | undefined;

  constructor(info: PartInfo) {
    super(info);
    if (info.type !== PartType.ELEMENT) throw new Error('Use tooltip() on an element.');
  }

  render(_text: unknown) {
    return noChange;
  }

  override update(part: ElementPart, [text]: [unknown]) {
    if (!this.element) {
      this.element = part.element;
      this.listen(true);
    }
    const next = typeof text === 'string' ? text : '';
    if (next !== this.text) {
      this.hide();
      this.text = next;
    }
    if (next) part.element.setAttribute('data-tooltip', next);
    else part.element.removeAttribute('data-tooltip');
    return noChange;
  }

  protected override disconnected(): void {
    this.hide();
    this.listen(false);
  }

  protected override reconnected(): void {
    this.listen(true);
  }

  private listen(add: boolean): void {
    const method = add ? 'addEventListener' : 'removeEventListener';
    this.element?.[method]('pointerenter', this.enter);
    this.element?.[method]('pointerleave', this.leave);
    this.element?.[method]('focusin', this.focus);
    this.element?.[method]('focusout', this.blur);
  }

  private readonly enter = (): void => {
    window.clearTimeout(this.hideTimer);
    this.hovered = true;
    this.show();
  };
  private readonly leave = (): void => {
    this.hovered = false;
    if (!this.focused) this.hideTimer = window.setTimeout(this.hide, 100);
  };
  private readonly focus = (): void => {
    this.focused = true;
    this.show();
  };
  private readonly blur = (event: Event): void => {
    const next = (event as FocusEvent).relatedTarget;
    if (next instanceof Node && this.element?.contains(next)) return;
    this.focused = false;
    if (!this.hovered) this.hide();
  };
  private readonly keydown = (event: KeyboardEvent): void => {
    // Dismiss help without consuming Escape from the containing menu or dialog.
    if (event.key === 'Escape') this.hide();
  };
  private readonly scroll = (event: Event): void => {
    if (event.target instanceof Node && this.popup?.contains(event.target)) return;
    if (this.focused) this.position();
    else this.hide();
  };
  private readonly hide = (): void => {
    window.clearTimeout(this.hideTimer);
    const popup = this.popup;
    if (!popup) return;
    const describedBy = this.element
      ?.getAttribute('aria-describedby')
      ?.split(/\s+/)
      .filter((id) => id !== popup.id)
      .join(' ');
    if (describedBy) this.element?.setAttribute('aria-describedby', describedBy);
    else this.element?.removeAttribute('aria-describedby');
    popup.remove();
    this.popup = undefined;
    if (dismissActive === this.hide) dismissActive = undefined;
    document.removeEventListener('keydown', this.keydown);
    document.removeEventListener('scroll', this.scroll, true);
    window.removeEventListener('resize', this.hide);
  };

  private show(): void {
    if (!this.text || !this.element?.isConnected || this.popup || !this.isConnected) return;
    dismissActive?.();
    const popup = document.createElement('div');
    popup.id = `opm-tooltip-${++nextId}`;
    popup.className = 'tooltip tooltip-open fixed pointer-events-none z-50';
    popup.setAttribute('popover', 'manual');
    // The popover is only a clipping escape; DaisyUI owns the content's visual styling.
    Object.assign(popup.style, {
      position: 'fixed',
      inset: 'auto',
      margin: '0',
      padding: '0',
      border: '0',
      overflow: 'visible',
      background: 'transparent',
      width: '0',
      height: '0',
    });
    const content = document.createElement('div');
    content.className = 'tooltip-content break-words';
    content.style.maxWidth = 'min(20rem, calc(100vw - 1rem))';
    content.style.maxHeight = 'calc(100vh - 2rem)';
    content.style.overflowY = 'auto';
    content.style.pointerEvents = 'auto';
    content.setAttribute('role', 'tooltip');
    content.textContent = this.text;
    content.addEventListener('pointerenter', this.enter);
    content.addEventListener('pointerleave', this.leave);
    popup.append(content);
    (this.element.closest('dialog[open]') ?? document.body).append(popup);
    if (typeof popup.showPopover === 'function') popup.showPopover();
    else popup.style.display = 'block';
    this.popup = popup;
    this.position();
    if (this.popup !== popup) return;
    const existing = this.element.getAttribute('aria-describedby');
    this.element.setAttribute('aria-describedby', [existing, popup.id].filter(Boolean).join(' '));
    dismissActive = this.hide;
    document.addEventListener('keydown', this.keydown);
    document.addEventListener('scroll', this.scroll, true);
    window.addEventListener('resize', this.hide);
  }

  private position(): void {
    const popup = this.popup;
    const content = popup?.firstElementChild;
    if (!popup || !content || !this.element) return;
    const anchor = this.element.getBoundingClientRect();
    if (anchor.bottom < 0 || anchor.top > window.innerHeight) {
      this.hide();
      return;
    }
    const area = this.element.closest('[role="menu"]')?.getBoundingClientRect() ?? anchor;
    const bounds = content.getBoundingClientRect();
    const above = area.top >= bounds.height + 16;
    popup.classList.toggle('tooltip-bottom', !above);
    const x = Math.max(
      bounds.width / 2 + 8,
      Math.min(window.innerWidth - bounds.width / 2 - 8, anchor.left + anchor.width / 2),
    );
    popup.style.left = `${x}px`;
    popup.style.top = `${above ? area.top : Math.max(0, Math.min(area.bottom, window.innerHeight - bounds.height - 16))}px`;
  }
}

export const tooltip = directive(TooltipDirective);
