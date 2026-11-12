import { html, nothing } from 'lit';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TaskKind } from '@/api/generated/TaskKind';
import type { TaskState } from '@/api/generated/TaskState';
import { OpmElement } from '@/base/opm-element';
import type { IconName } from '@/components/opm-icon';
import '@/components/opm-icon';
import { taskFraction } from '@/stores/tasks';
import { formatEta, formatFileSize, formatRate } from '@/util/format';
import { formatCount } from '@/util/format-count';

export interface TaskRowDetail {
  id: number;
}

export type TaskCancelEvent = CustomEvent<TaskRowDetail>;
export type TaskDismissEvent = CustomEvent<TaskRowDetail>;

// A record, so a task kind added to the backend fails the type check until it has an icon here.
const KIND_ICON: Readonly<Record<TaskKind, IconName>> = {
  index: 'database',
  fs_move: 'folder-input',
  fs_trash: 'trash-2',
  fs_rmdir: 'folder-x',
  plan_execute: 'list-checks',
  immich_sync: 'cloud-download',
  immich_cleanup: 'cloud-upload',
  trash_empty: 'brush-cleaning',
};

/** Tailwind needs these as complete strings. */
const DOT_CLASS: Readonly<Record<TaskState, string>> = {
  queued: 'status-neutral',
  running: 'status-primary motion-safe:animate-pulse',
  done: 'status-success',
  failed: 'status-error',
  cancelled: 'status-warning',
  interrupted: 'status-error',
};

type EndedState = Exclude<TaskState, 'queued' | 'running'>;

// A queued task has no bar.
const BAR_CLASS: Readonly<Record<Exclude<TaskState, 'queued'>, string>> = {
  running: 'progress-primary',
  done: 'progress-success',
  failed: 'progress-error',
  cancelled: 'progress-warning',
  interrupted: 'progress-error',
};

/** How a task that is over is announced in its numbers line. */
const ENDED_LABEL: Readonly<Record<EndedState, string>> = {
  done: 'Done',
  failed: 'Failed',
  cancelled: 'Cancelled',
  interrupted: 'Interrupted',
};

const WAITING = 'Waiting';
const CANCELLING = 'Cancelling…';
const STARTING = 'Starting…';

/** The `<progress>` value for a fraction: a tenth of a percent is finer than any bar shows. */
function percent(fraction: number): number {
  return Math.round(fraction * 1000) / 10;
}

function itemsText(task: TaskDto): string | null {
  const { items_done: done, items_total: total } = task;
  if (total === null && done <= 0) return null;
  const unit = (total ?? done) === 1 ? 'item' : 'items';
  // A finished task has done everything it set out to do: "1,204 items" says it better.
  if (total !== null && !(task.state === 'done' && done === total)) {
    return `${formatCount(done)} / ${formatCount(total)} ${unit}`;
  }
  return `${formatCount(done)} ${unit}`;
}

function bytesText(task: TaskDto): string | null {
  const { bytes_done: done, bytes_total: total } = task;
  if (total !== null && !(task.state === 'done' && done === total)) {
    return `${formatFileSize(done)} / ${formatFileSize(total)}`;
  }
  return total === null && done <= 0 ? null : formatFileSize(done);
}

/**
 * One task of the task indicator: its state, kind, title, progress bar and one line of numbers.
 *
 *   html`<opm-task-row
 *     .task=${task}
 *     .cancelling=${cancelling.value.has(task.id)}
 *     @task-cancel=${(e: TaskCancelEvent) => cancel(e.detail.id)}
 *     @task-dismiss=${(e: TaskDismissEvent) => dismiss(e.detail.id)}
 *   ></opm-task-row>`
 *
 * It only shows what it is given: it reads no store and cancels nothing itself. Events (both
 * bubble, `detail` is `{ id }`): `task-cancel` from the cancel button of a queued or running task,
 * `task-dismiss` from the dismiss button of a task that is over.
 *
 * The title is the server's text and changes while a task runs: it is rendered as text, never as
 * markup, truncated to one line with the whole text in its native tooltip. The bar is determinate
 * once the task declared a total (bytes when it has a byte total, else items; see `taskFraction`),
 * indeterminate while a running task has none, and absent for a queued task. The numbers line
 * shows the counts and, for a running task, the speed (bytes per second when the task has a byte
 * total, else items per second) and the time left. `cancelling` replaces them with "Cancelling…"
 * and disables the cancel button. A failed or interrupted task also shows its error text.
 *
 * `compact` is for a narrow spot: tighter spacing, no kind icon, no speed and no time left.
 */
export class OpmTaskRow extends OpmElement {
  static override properties = {
    task: { attribute: false },
    cancelling: { type: Boolean },
    compact: { type: Boolean },
  };

  declare task: TaskDto | undefined;
  /** The user asked to cancel this task and it has not ended yet. */
  declare cancelling: boolean;
  declare compact: boolean;

  constructor() {
    super();
    this.cancelling = false;
    this.compact = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
  }

  private emit(type: 'task-cancel' | 'task-dismiss', id: number): void {
    this.dispatchEvent(new CustomEvent<TaskRowDetail>(type, { detail: { id }, bubbles: true }));
  }

  /** The numbers line, one entry per segment (a segment is never split over two lines). */
  private segments(task: TaskDto): string[] {
    if (task.state === 'queued') return [this.cancelling ? CANCELLING : WAITING];
    const numbers = [itemsText(task), bytesText(task)].filter((text) => text !== null);
    if (task.state !== 'running') return [ENDED_LABEL[task.state], ...numbers];
    if (this.cancelling) return [CANCELLING, ...numbers];

    if (!this.compact) {
      if (task.speed !== null && task.speed > 0) {
        numbers.push(formatRate(task.speed, task.bytes_total !== null ? 'bytes' : 'items'));
      }
      if (task.eta_secs !== null) numbers.push(`${formatEta(task.eta_secs)} left`);
    }
    return numbers.length > 0 ? numbers : [STARTING];
  }

  private renderBar(task: TaskDto, fraction: number | null) {
    if (task.state === 'queued') return nothing;
    const colour = BAR_CLASS[task.state];
    if (task.state === 'done') {
      return html`<progress class="progress ${colour} w-full" value="100" max="100" aria-label=${task.title}></progress>`;
    }
    if (fraction === null) {
      // Without a total only a running task has something to show: a bar that moves.
      if (task.state !== 'running') return nothing;
      return html`<progress class="progress ${colour} w-full" aria-label=${task.title}></progress>`;
    }
    return html`<progress
      class="progress ${colour} w-full"
      value=${percent(fraction)}
      max="100"
      aria-label=${task.title}
    ></progress>`;
  }

  private renderAction(task: TaskDto) {
    if (task.state === 'queued' || task.state === 'running') {
      return html`<button
        type="button"
        class="btn btn-ghost btn-square btn-xs"
        title="Cancel"
        aria-label="Cancel ${task.title}"
        ?disabled=${this.cancelling}
        @click=${() => this.emit('task-cancel', task.id)}
      >
        <opm-icon name="x" size="sm"></opm-icon>
      </button>`;
    }
    return html`<button
      type="button"
      class="btn btn-ghost btn-square btn-xs"
      title="Dismiss"
      aria-label="Dismiss ${task.title}"
      @click=${() => this.emit('task-dismiss', task.id)}
    >
      <opm-icon name="x" size="sm"></opm-icon>
    </button>`;
  }

  protected override render() {
    const { task, compact } = this;
    if (!task) return nothing;
    const over = task.state !== 'queued' && task.state !== 'running';
    const failed = task.state === 'failed' || task.state === 'interrupted';
    return html`
      <div
        class="flex flex-col ${compact ? 'gap-1 px-2 py-1' : 'gap-1.5 px-3 py-2'}"
        data-state=${task.state}
      >
        <div class="flex items-center gap-2">
          <span class="status shrink-0 ${DOT_CLASS[task.state]}" aria-hidden="true"></span>
          ${
            compact
              ? nothing
              : html`<opm-icon
                  name=${KIND_ICON[task.kind]}
                  size="sm"
                  class="text-base-content/60"
                ></opm-icon>`
          }
          <span
            class="min-w-0 flex-1 truncate font-medium ${compact ? 'text-xs' : 'text-sm'}"
            title=${task.title}
            >${task.title}</span
          >
          ${this.renderAction(task)}
        </div>
        ${this.renderBar(task, taskFraction(task))}
        <div class="flex flex-wrap gap-x-3 text-xs tabular-nums text-base-content/70">
          ${this.segments(task).map(
            (text, index) =>
              html`<span class="whitespace-nowrap ${over && index === 0 ? 'font-medium text-base-content' : ''}">${text}</span>`,
          )}
        </div>
        ${
          failed && task.error
            ? html`<div class="alert alert-error alert-soft justify-items-start px-2 py-1 text-xs">
                <span class="min-w-0 wrap-anywhere">${task.error}</span>
              </div>`
            : nothing
        }
      </div>
    `;
  }
}

customElements.define('opm-task-row', OpmTaskRow);

declare global {
  interface HTMLElementTagNameMap {
    'opm-task-row': OpmTaskRow;
  }
  interface HTMLElementEventMap {
    'task-cancel': TaskCancelEvent;
    'task-dismiss': TaskDismissEvent;
  }
}
