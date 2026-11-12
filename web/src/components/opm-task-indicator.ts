import { html, nothing } from 'lit';
import { ifDefined } from 'lit/directives/if-defined.js';
import { createRef, ref } from 'lit/directives/ref.js';
import { repeat } from 'lit/directives/repeat.js';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TaskState } from '@/api/generated/TaskState';
import { OpmElement } from '@/base/opm-element';
import type { TaskCancelEvent, TaskDismissEvent } from '@/components/opm-task-row';
import '@/components/opm-task-row';
import {
  aggregate,
  cancel,
  cancelling,
  dismiss,
  dismissFinished,
  isEnded,
  onTaskFinished,
  type TaskAggregate,
  tasks,
} from '@/stores/tasks';

let instanceCounter = 0;
let announcementCounter = 0;

/** The live region keeps the last few announcements, so several tasks ending at once are all read. */
const MAX_ANNOUNCEMENTS = 5;

interface Announcement {
  key: number;
  text: string;
}

// A record, so a state added to the backend enum fails the type check until it has a phrase here.
const ENDED_PHRASE: Readonly<Record<Exclude<TaskState, 'queued' | 'running'>, string>> = {
  done: 'finished',
  failed: 'failed',
  cancelled: 'was cancelled',
  interrupted: 'was interrupted',
};

interface TriggerView {
  /** Short text next to the bar (from the `md` breakpoint up). */
  label: string;
  /** The accessible name of the trigger: the same in words. It always contains `label`. */
  name: string;
  /** The colour class of the bar. */
  tone: string;
  /** 0 to 100, or `null` for an indeterminate bar. */
  value: number | null;
  /** The share shown beside the bar: only while a running task reports one. */
  percent: number | null;
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** What the trigger says about the visible tasks. */
function triggerView(list: readonly TaskDto[], counts: TaskAggregate): TriggerView {
  const { running, queued, failed, fraction } = counts;
  const percent = fraction === null ? null : Math.round(fraction * 100);
  const parts: string[] = [];
  let label: string;
  let tone: string;
  let value: number | null;

  if (running > 0) {
    const only = running === 1 ? list.find((task) => task.state === 'running') : undefined;
    label = only ? only.title : plural(running, 'task', 'tasks');
    parts.push(`${label} running`);
    if (percent !== null) parts.push(`${percent} percent`);
    tone = 'progress-primary';
    value = percent;
  } else if (queued > 0) {
    label = 'Waiting';
    tone = 'progress-primary';
    value = 0;
  } else if (failed > 0) {
    label = 'Failed';
    tone = 'progress-error';
    value = 100;
  } else {
    // Only tasks that ended well are left, for the few seconds they stay listed.
    const cancelledOnly = list.every((task) => task.state === 'cancelled');
    label = cancelledOnly ? 'Cancelled' : 'Done';
    tone = cancelledOnly ? 'progress-warning' : 'progress-success';
    value = 100;
  }
  if (queued > 0) parts.push(`${queued} waiting`);
  if (failed > 0) parts.push(`${failed} failed`);
  if (parts.length === 0) parts.push(`Tasks ${label.toLowerCase()}`);
  return { label, name: parts.join(', '), tone, value, percent };
}

/**
 * The background-task indicator of the shell's top bar. It has no properties and no events: the
 * shell starts the tasks store (`startTasks()`), this element only shows it.
 *
 *   <opm-task-indicator></opm-task-indicator>
 *
 * It renders NOTHING (and takes no room in the bar) while no task is visible. Otherwise it is one
 * compact button: a bar of fixed width (narrower below the `md` breakpoint, to leave room for the
 * page heading on a phone; determinate once a running task declared a total,
 * indeterminate before), the share in tabular digits, a short label (the title of the only running
 * task, "N tasks", "Waiting", "Failed" or "Done"; from the `md` breakpoint up), a red badge with
 * the number of failed tasks and a grey one with the number of queued tasks. Its accessible name
 * says it in words ("2 tasks running, 62 percent, 1 failed"). The bar and the share never change
 * the width of the button, so the bar around it does not move while a task progresses.
 *
 * Hover and keyboard focus open the panel with one `opm-task-row` per visible task, and it stays
 * open while the pointer is inside it. A click (or Enter or Space) pins it open until the next
 * click, an outside press, Escape or the focus leaving; that is how a touch screen opens it.
 * Escape closes it from anywhere on the page (not only from inside) and is then not passed on, so
 * the page's own Escape shortcut does not also run; the focus goes back to the trigger when it was
 * inside the panel. The panel opens again on the next hover or focus. A "Clear finished" button
 * dismisses every task that is over.
 *
 * From the `md` breakpoint up the panel (24rem wide) hangs from the right edge of the trigger.
 * Below it the panel is a sheet fixed to the screen, 0.5rem from each side and just under the
 * trigger: on a phone the trigger is not flush with the right edge (the theme toggle follows it),
 * so a panel hung from it would stick out of the screen on the left.
 *
 * The open state is driven here, with `dropdown-open` and `dropdown-close`, rather than by
 * DaisyUI's own `dropdown-hover` and `:focus-within`: the CSS alone cannot be dismissed with
 * Escape or leave the panel pinned, and the state has to be in the DOM to be tested. The trigger
 * is a disclosure button (`aria-expanded`, `aria-controls`); the panel is not a menu.
 * Its foreground and background switch themes together without colour transitions, which would
 * briefly blend them into low-contrast midtones while the focused button changes theme.
 *
 * ONE visually hidden live region (`aria-live="polite"`, `aria-atomic="false"` so an addition does
 * not make a screen reader read the earlier lines again) announces the tasks that end while the
 * page is open ("Move 120 files finished", "Indexing failed"). Progress is never announced.
 */
export class OpmTaskIndicator extends OpmElement {
  static override properties = {
    pinned: { state: true },
    hovered: { state: true },
    focused: { state: true },
    suppressed: { state: true },
    announcements: { state: true },
  };

  /** Opened by a click: stays open until closed. */
  declare private pinned: boolean;
  declare private hovered: boolean;
  declare private focused: boolean;
  /** Closed by Escape or a second click: stays closed until the next hover or focus. */
  declare private suppressed: boolean;
  declare private announcements: Announcement[];

  private readonly panelId = `opm-tasks-${++instanceCounter}`;
  private readonly triggerRef = createRef<HTMLButtonElement>();
  private stopFinished: (() => void) | undefined;
  private listening = false;

  constructor() {
    super();
    this.pinned = false;
    this.hovered = false;
    this.focused = false;
    this.suppressed = false;
    this.announcements = [];
  }

  override connectedCallback(): void {
    super.connectedCallback();
    // Without a box of its own an idle indicator is not an (empty) item of the bar's flex gap.
    this.classList.add('contents');
    this.stopFinished = onTaskFinished(this.onFinished);
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.stopFinished?.();
    this.stopFinished = undefined;
    this.resetInteraction();
    this.syncDocumentListeners(false);
  }

  protected override willUpdate(): void {
    if (tasks.value.length > 0) return;
    this.resetInteraction();
    this.announcements = [];
  }

  protected override updated(): void {
    this.syncDocumentListeners(this.isOpen);
  }

  private get isOpen(): boolean {
    return this.pinned || (!this.suppressed && (this.hovered || this.focused));
  }

  private resetInteraction(): void {
    this.pinned = false;
    this.hovered = false;
    this.focused = false;
    this.suppressed = false;
  }

  /** Escape and outside presses only matter, and are only listened to, while the panel is open. */
  private syncDocumentListeners(open: boolean): void {
    if (open === this.listening) return;
    this.listening = open;
    if (open) {
      document.addEventListener('pointerdown', this.onOutsidePointerDown, true);
      document.addEventListener('keydown', this.onEscape, true);
    } else {
      document.removeEventListener('pointerdown', this.onOutsidePointerDown, true);
      document.removeEventListener('keydown', this.onEscape, true);
    }
  }

  private readonly onFinished = (task: TaskDto): void => {
    if (task.state === 'queued' || task.state === 'running') return;
    const text = `${task.title} ${ENDED_PHRASE[task.state]}`;
    this.announcements = [...this.announcements, { key: ++announcementCounter, text }].slice(
      -MAX_ANNOUNCEMENTS,
    );
  };

  // A touch emulates enter and leave around a tap: only a real pointer hovers.
  private readonly onPointerEnter = (event: PointerEvent): void => {
    if (event.pointerType === 'touch') return;
    this.hovered = true;
    this.suppressed = false;
  };

  private readonly onPointerLeave = (event: PointerEvent): void => {
    if (event.pointerType === 'touch') return;
    this.hovered = false;
  };

  private readonly onFocusIn = (event: FocusEvent): void => {
    const from = event.relatedTarget;
    if (!(from instanceof Node && this.contains(from))) this.suppressed = false;
    this.focused = true;
  };

  private readonly onFocusOut = (event: FocusEvent): void => {
    const to = event.relatedTarget;
    if (to instanceof Node && this.contains(to)) return;
    this.focused = false;
    // Focus that goes to nothing (a click on bare page, the window losing focus) keeps a pinned
    // panel open: only a move to another element, or an outside press, lets go of it.
    if (to instanceof Node) this.pinned = false;
  };

  private readonly onTriggerClick = (): void => {
    if (this.pinned) {
      this.pinned = false;
      this.suppressed = true;
    } else {
      this.pinned = true;
      this.suppressed = false;
    }
  };

  /** Capture phase: a handler elsewhere cannot swallow the press. */
  private readonly onOutsidePointerDown = (event: Event): void => {
    const target = event.target;
    if (target instanceof Node && this.contains(target)) return;
    this.pinned = false;
    this.focused = false;
  };

  private readonly onEscape = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing) return;
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    // Escape belongs to an open modal dialog.
    if (document.querySelector('dialog[open]')) return;
    event.preventDefault();
    event.stopPropagation();
    const inside = this.contains(document.activeElement);
    this.pinned = false;
    this.suppressed = true;
    if (inside) this.triggerRef.value?.focus();
  };

  /**
   * A button of the panel can disappear under the user's focus (a dismissed row, a disabled
   * cancel button): hand the focus to the trigger instead of letting it fall to the page.
   */
  private keepFocus(action: () => void): void {
    const inside = this.contains(document.activeElement);
    action();
    if (!inside) return;
    void this.updateComplete.then(() => {
      const active = document.activeElement;
      if (!active || active === document.body || !active.isConnected) {
        this.triggerRef.value?.focus();
      }
    });
  }

  private readonly onRowCancel = (event: TaskCancelEvent): void => {
    event.stopPropagation();
    this.keepFocus(() => void cancel(event.detail.id));
  };

  private readonly onRowDismiss = (event: TaskDismissEvent): void => {
    event.stopPropagation();
    this.keepFocus(() => dismiss(event.detail.id));
  };

  private readonly onClearFinished = (): void => {
    this.keepFocus(dismissFinished);
  };

  private renderBadges(counts: TaskAggregate) {
    return html`
      ${
        counts.failed > 0
          ? html`<span
              class="badge badge-error badge-sm tabular-nums"
              title="${counts.failed} failed"
              >${counts.failed}</span
            >`
          : nothing
      }
      ${
        counts.queued > 0
          ? html`<span
              class="badge badge-ghost badge-sm tabular-nums"
              title="${counts.queued} waiting"
              >${counts.queued}</span
            >`
          : nothing
      }
    `;
  }

  private renderPanel(list: readonly TaskDto[]) {
    const cancellingIds = cancelling.value;
    return html`
      <div id=${this.panelId} class="dropdown-content pt-2 max-md:fixed max-md:inset-x-2">
        <div
          role="group"
          aria-label="Background tasks"
          class="card w-full border border-base-300 bg-base-100 shadow-lg md:w-96"
        >
          <div
            class="flex items-center justify-between gap-2 border-b border-base-300 py-1 pr-2 pl-3"
          >
            <span class="text-sm font-semibold">Background tasks</span>
            ${
              list.some(isEnded)
                ? html`<button
                    type="button"
                    class="btn btn-ghost btn-xs"
                    @click=${this.onClearFinished}
                  >
                    Clear finished
                  </button>`
                : nothing
            }
          </div>
          <ul
            class="max-h-[70vh] divide-y divide-base-300 overflow-y-auto"
            @task-cancel=${this.onRowCancel}
            @task-dismiss=${this.onRowDismiss}
          >
            ${repeat(
              list,
              (task) => task.id,
              (task) => html`<li>
                <opm-task-row
                  .task=${task}
                  .cancelling=${cancellingIds.has(task.id)}
                ></opm-task-row>
              </li>`,
            )}
          </ul>
        </div>
      </div>
    `;
  }

  protected override render() {
    const list = tasks.value;
    if (list.length === 0) return nothing;
    const counts = aggregate.value;
    const view = triggerView(list, counts);
    const open = this.isOpen;
    return html`
      <div
        class="dropdown dropdown-end ${open ? 'dropdown-open' : 'dropdown-close'}"
        @pointerenter=${this.onPointerEnter}
        @pointerleave=${this.onPointerLeave}
        @focusin=${this.onFocusIn}
        @focusout=${this.onFocusOut}
      >
        <button
          ${ref(this.triggerRef)}
          type="button"
          class="btn btn-ghost btn-sm gap-2 px-2 transition-[border-color,box-shadow,transform]"
          aria-label=${view.name}
          aria-expanded=${open ? 'true' : 'false'}
          aria-controls=${this.panelId}
          @click=${this.onTriggerClick}
        >
          <progress
            class="progress ${view.tone} w-16 md:w-28"
            max="100"
            value=${ifDefined(view.value ?? undefined)}
          ></progress>
          ${
            counts.running > 0
              ? html`<span class="w-9 text-right text-xs tabular-nums"
                  >${view.percent === null ? nothing : `${view.percent}%`}</span
                >`
              : nothing
          }
          <span class="hidden max-w-40 truncate text-sm font-normal md:inline">${view.label}</span>
          ${this.renderBadges(counts)}
        </button>
        ${this.renderPanel(list)}
      </div>
      <div class="sr-only" role="status" aria-live="polite" aria-atomic="false">
        ${repeat(
          this.announcements,
          (announcement) => announcement.key,
          (announcement) => html`<p>${announcement.text}</p>`,
        )}
      </div>
    `;
  }
}

customElements.define('opm-task-indicator', OpmTaskIndicator);

declare global {
  interface HTMLElementTagNameMap {
    'opm-task-indicator': OpmTaskIndicator;
  }
}
