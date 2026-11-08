import { html, nothing, type TemplateResult } from 'lit';
import { ifDefined } from 'lit/directives/if-defined.js';
import { errorMessage, isAbortError, isApiError } from '@/api/errors';
import type { SortOrder } from '@/api/generated/SortOrder';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TaskKind } from '@/api/generated/TaskKind';
import type { TrashBatchDto } from '@/api/generated/TrashBatchDto';
import type { TrashBatchesResponse } from '@/api/generated/TrashBatchesResponse';
import type { TrashEmptyRequest } from '@/api/generated/TrashEmptyRequest';
import type { TrashItemDto } from '@/api/generated/TrashItemDto';
import type { TrashListResponse } from '@/api/generated/TrashListResponse';
import type { TrashSort } from '@/api/generated/TrashSort';
import type { TrashSummary } from '@/api/generated/TrashSummary';
import { events, type ServerEventOf } from '@/api/sse';
import { emptyTrash, listTrash, listTrashBatches, restoreTrash, syncTrash } from '@/api/trash';
import { ApmElement } from '@/base/apm-element';
import { confirmDialog } from '@/components/confirm-dialog';
import { toast } from '@/components/toast';
import { linkTo } from '@/stores/nav';
import { isEnded } from '@/stores/tasks';
import type { TrashBatchEvent } from '@/tools/trash/apm-trash-deletions';
import type {
  TrashIdsDetail,
  TrashPageDetail,
  TrashSearchDetail,
  TrashSelectDetail,
  TrashSelectPageDetail,
  TrashSortDetail,
  TrashVolumeDetail,
} from '@/tools/trash/apm-trash-items';
import {
  renderEmpty,
  renderPager,
  renderSkeleton,
  renderVolumeBadges,
  renderVolumeSelect,
} from '@/tools/trash/trash-parts';
import {
  blockedReason,
  emptyBlockedReason,
  emptyConfirmation,
  emptyScope,
  lastPageOffset,
  restoreBatchConfirmation,
  retentionText,
  taskOutcome,
} from '@/tools/trash/trash-text';
import { EM_DASH, formatFileSize, formatFileTime } from '@/util/format';
import { formatCount } from '@/util/format-count';
import '@/components/apm-icon';
import type { IconName } from '@/components/apm-icon';
import '@/tools/trash/apm-trash-deletions';
import '@/tools/trash/apm-trash-items';

/** Rows per page on both tabs. The server takes 1 to 200 per request. */
export const PAGE_SIZE = 50;

/** How long the search box must rest before the list follows what was typed. */
export const SEARCH_DEBOUNCE_MS = 300;

/** Task events that arrive together (a restore of many items) become one refetch, this long after the first. */
export const REFRESH_DEBOUNCE_MS = 500;

/** How many recent task endings are kept for a task whose start the page has not seen yet. */
const SEEN_LIMIT = 20;

const ELLIPSIS = '\u2026';

/**
 * The tasks whose ending can change the trash: a restore (`fs_move`), what trashes or checks
 * (`fs_trash`, `plan_execute`) and emptying. An `index` task is not one: scans never touch the
 * trash, and the check of the trash is an `fs_trash` task.
 */
const REFRESH_KINDS: ReadonlySet<TaskKind> = new Set<TaskKind>([
  'fs_move',
  'fs_trash',
  'plan_execute',
  'trash_empty',
]);

/** The direction a column sorts in until the user flips it: newest and largest first, names A to Z. */
const NATURAL_ORDER: Readonly<Record<TrashSort, SortOrder>> = {
  trashed_at: 'desc',
  size: 'desc',
  name: 'asc',
  path: 'asc',
};

type Tab = 'deletions' | 'items';

const TABS: readonly { id: Tab; label: string; icon: IconName }[] = [
  { id: 'deletions', label: 'Deletions', icon: 'clock' },
  { id: 'items', label: 'Items', icon: 'list' },
];

const tabId = (tab: Tab) => `trash-tab-${tab}`;
const panelId = (tab: Tab) => `trash-panel-${tab}`;

interface TabStatus {
  loading: boolean;
  error: string | null;
}

const IDLE: TabStatus = { loading: false, error: null };

/** The deletion the Items tab is narrowed to; it lives on `volumeId`. */
interface BatchFilter {
  volumeId: number;
  batch: string;
  /** The text of the chip. */
  label: string;
}

/** DaisyUI's focus dropdown has no Escape: take the focus off the trigger, which closes it. */
function closeOnEscape(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return;
  const active = document.activeElement;
  const area = event.currentTarget;
  if (active instanceof HTMLElement && area instanceof Node && area.contains(active)) {
    event.preventDefault();
    event.stopPropagation();
    active.blur();
  }
}

function closeMenu(): void {
  const active = document.activeElement;
  if (active instanceof HTMLElement) active.blur();
}

/**
 * The Trash route: what the trash holds, what was deleted when, and the only two ways out of it:
 * restore (back to the original paths) and "Empty trash…" (permanent, always confirmed).
 *
 * A header (totals, the volumes that hold trash, "Empty trash…", the retention rule) and two
 * tabs. "Deletions" lists one row per deletion (a batch), newest first, with "Restore all" and
 * "View items". "Items" lists the files and folders, with search, a volume filter, sortable
 * columns and a selection to restore. Both are paged by `PAGE_SIZE`.
 *
 * Loading: "Deletions" loads when the page opens, "Items" the first time it is opened. A reload
 * keeps the rows on screen (the skeleton is only for a first load), a failure shows an alert with
 * Retry, and a newer request supersedes an older one. Every answer carries the global `summary`
 * (totals, volumes, retention, the highest item id), which the header and the "Empty trash…"
 * confirmation read. The page refetches when a restore, trashing, plan or emptying task ends
 * (500 ms debounce), after the event stream reconnects and when the window regains focus.
 *
 * The page does the work and the tabs only draw: every request, the selection, the confirmations
 * and the toasts are here. A restore or an emptying is a task: the page toasts its start and its
 * outcome (the task indicator of the shell shows the progress).
 *
 * "Empty trash…" asks for `summary.max_id` as it was when the dialog opened (`up_to_id`), so
 * nothing trashed while the user reads the question is deleted. It is the only permanent delete.
 *
 * The shell owns the page's `<h1>`, so the heading here is an `<h2>`.
 */
export class ApmTrashPage extends ApmElement {
  static override properties = {
    tab: { state: true },
    summary: { state: true },
    batches: { state: true },
    list: { state: true },
    status: { state: true },
    volumeId: { state: true },
    batchFilter: { state: true },
    searchDraft: { state: true },
    q: { state: true },
    sort: { state: true },
    order: { state: true },
    offset: { state: true },
    batchOffset: { state: true },
    selected: { state: true },
    busy: { state: true },
  };

  declare tab: Tab;
  /** The global numbers of the newest answer; `null` until the first one. */
  declare summary: TrashSummary | null;
  declare batches: TrashBatchesResponse | null;
  declare list: TrashListResponse | null;
  declare status: Readonly<Record<Tab, TabStatus>>;
  /** The volume both tabs are narrowed to, `null` for all of them. */
  declare volumeId: number | null;
  declare batchFilter: BatchFilter | null;
  /** The text in the search box, and `q` the text the list was asked for (trimmed, after the pause). */
  declare searchDraft: string;
  declare q: string;
  declare sort: TrashSort;
  /** `null` is the natural direction of `sort`. */
  declare order: SortOrder | null;
  /** The first row of the page of each tab. */
  declare offset: number;
  declare batchOffset: number;
  /** Item ids on the page of the Items tab. */
  declare selected: ReadonlySet<number>;
  /** A confirmation or a request of the user is under way: the action buttons wait. */
  declare busy: boolean;

  /** The tabs that must reload before they are shown again. */
  private readonly stale = new Set<Tab>();
  private readonly controllers: Record<Tab, AbortController | null> = {
    deletions: null,
    items: null,
  };
  /** The tasks this page started and has not seen end, to toast their outcome. */
  private readonly started = new Set<number>();
  /** Recent endings of tasks that change the trash, for a task that ended before its start was answered. */
  private readonly seen = new Map<number, TaskDto>();
  private searchTimer: number | undefined;
  private refreshTimer: number | undefined;
  private unsubscribe: (() => void)[] = [];

  constructor() {
    super();
    this.tab = 'deletions';
    this.summary = null;
    this.batches = null;
    this.list = null;
    this.status = { deletions: IDLE, items: IDLE };
    this.volumeId = null;
    this.batchFilter = null;
    this.searchDraft = '';
    this.q = '';
    this.sort = 'trashed_at';
    this.order = null;
    this.offset = 0;
    this.batchOffset = 0;
    this.selected = new Set();
    this.busy = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
    this.unsubscribe = [events.on('task', this.onTask), events.onResync(this.scheduleRefresh)];
    window.addEventListener('focus', this.scheduleRefresh);
    this.refresh();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    for (const off of this.unsubscribe) off();
    this.unsubscribe = [];
    window.removeEventListener('focus', this.scheduleRefresh);
    window.clearTimeout(this.searchTimer);
    window.clearTimeout(this.refreshTimer);
    this.searchTimer = undefined;
    this.refreshTimer = undefined;
    for (const { id } of TABS) this.cancel(id);
  }

  // ---- loading -------------------------------------------------------------------------------

  private cancel(tab: Tab): void {
    this.controllers[tab]?.abort();
    this.controllers[tab] = null;
  }

  private setStatus(tab: Tab, status: TabStatus): void {
    this.status = { ...this.status, [tab]: status };
  }

  private load(tab: Tab): void {
    if (tab === 'deletions') void this.loadDeletions();
    else void this.loadItems();
  }

  /** Reloads the open tab now and the other one when it is opened. */
  private refresh(): void {
    this.stale.add(this.tab === 'deletions' ? 'items' : 'deletions');
    this.load(this.tab);
  }

  /** The filters changed: the rows of both tabs are out of date. The open tab reloads now. */
  private invalidate(): void {
    for (const { id } of TABS) {
      this.cancel(id);
      this.stale.add(id);
    }
    this.status = { deletions: IDLE, items: IDLE };
    this.load(this.tab);
  }

  private ensureLoaded(tab: Tab): void {
    const loaded = tab === 'deletions' ? this.batches : this.list;
    if (this.stale.has(tab) || (loaded === null && !this.status[tab].loading)) this.load(tab);
  }

  /**
   * Sends the request of a tab and supersedes the one in flight. Resolves `null` when the answer
   * must be ignored: it was superseded or aborted, or it failed (the tab then shows the error).
   */
  private async fetchTab<T extends { summary: TrashSummary }>(
    tab: Tab,
    request: (signal: AbortSignal) => Promise<T>,
  ): Promise<T | null> {
    this.cancel(tab);
    const controller = new AbortController();
    this.controllers[tab] = controller;
    this.stale.delete(tab);
    this.setStatus(tab, { loading: true, error: null });
    try {
      const response = await request(controller.signal);
      if (this.controllers[tab] !== controller) return null;
      this.summary = response.summary;
      return response;
    } catch (error) {
      if (this.controllers[tab] === controller && !isAbortError(error)) {
        this.setStatus(tab, {
          loading: false,
          error: errorMessage(error, 'Could not reach the server'),
        });
      }
      return null;
    }
  }

  private async loadDeletions(): Promise<void> {
    const response = await this.fetchTab('deletions', (signal) =>
      listTrashBatches(
        { volume_id: this.volumeId, offset: this.batchOffset, limit: PAGE_SIZE },
        signal,
      ),
    );
    if (response === null || this.dropVanishedVolume(response.summary)) return;
    if (response.batches.length === 0 && this.batchOffset >= response.total && response.total > 0) {
      this.batchOffset = lastPageOffset(response.total, PAGE_SIZE);
      this.load('deletions');
      return;
    }
    this.batches = response;
    this.setStatus('deletions', IDLE);
  }

  private async loadItems(): Promise<void> {
    const response = await this.fetchTab('items', (signal) =>
      listTrash(
        {
          volume_id: this.batchFilter?.volumeId ?? this.volumeId,
          batch: this.batchFilter?.batch ?? null,
          q: this.q === '' ? null : this.q,
          sort: this.sort,
          order: this.order,
          offset: this.offset,
          limit: PAGE_SIZE,
        },
        signal,
      ),
    );
    if (response === null || this.dropVanishedVolume(response.summary)) return;
    if (response.items.length === 0 && this.offset >= response.total && response.total > 0) {
      this.offset = lastPageOffset(response.total, PAGE_SIZE);
      this.load('items');
      return;
    }
    const present = new Set(response.items.map((item) => item.id));
    this.selected = new Set([...this.selected].filter((id) => present.has(id)));
    this.list = response;
    this.setStatus('items', IDLE);
  }

  /** A volume that no longer holds trash cannot stay a filter: back to all volumes. */
  private dropVanishedVolume(summary: TrashSummary): boolean {
    const { volumeId } = this;
    if (volumeId === null || summary.volumes.some((volume) => volume.volume_id === volumeId)) {
      return false;
    }
    this.setVolume(null);
    return true;
  }

  // ---- filters, sorting, paging ----------------------------------------------------------------

  private resetPages(): void {
    this.offset = 0;
    this.batchOffset = 0;
    this.selected = new Set();
  }

  private cancelSearch(): void {
    window.clearTimeout(this.searchTimer);
    this.searchTimer = undefined;
  }

  private setVolume(volumeId: number | null): void {
    if (volumeId === this.volumeId && this.batchFilter === null) return;
    this.volumeId = volumeId;
    this.batchFilter = null;
    this.resetPages();
    this.invalidate();
  }

  /** Back to every deletion of every volume, with no search. */
  private clearFilters(): void {
    this.cancelSearch();
    this.searchDraft = '';
    this.q = '';
    this.volumeId = null;
    this.batchFilter = null;
    this.resetPages();
    this.invalidate();
  }

  private clearBatch(): void {
    this.batchFilter = null;
    this.offset = 0;
    this.selected = new Set();
    this.load('items');
  }

  private viewBatch(batch: TrashBatchDto): void {
    const volumeChanged = this.volumeId !== batch.volume_id;
    this.cancelSearch();
    this.searchDraft = '';
    this.q = '';
    this.volumeId = batch.volume_id;
    this.batchFilter = {
      volumeId: batch.volume_id,
      batch: batch.batch,
      label: `Deletion of ${formatFileTime(batch.trashed_at)} on ${batch.volume}`,
    };
    this.offset = 0;
    this.selected = new Set();
    if (volumeChanged) {
      this.batchOffset = 0;
      this.stale.add('deletions');
    }
    this.tab = 'items';
    this.load('items');
  }

  private onSearch(value: string): void {
    this.searchDraft = value;
    this.cancelSearch();
    this.searchTimer = window.setTimeout(() => {
      this.searchTimer = undefined;
      const q = value.trim();
      if (q === this.q) return;
      this.q = q;
      this.offset = 0;
      this.selected = new Set();
      this.load('items');
    }, SEARCH_DEBOUNCE_MS);
  }

  private get effectiveOrder(): SortOrder {
    return this.order ?? NATURAL_ORDER[this.sort];
  }

  private onSort({ sort }: TrashSortDetail): void {
    if (sort === this.sort) {
      this.order = this.effectiveOrder === 'asc' ? 'desc' : 'asc';
    } else {
      this.sort = sort;
      this.order = null;
    }
    this.offset = 0;
    this.selected = new Set();
    this.load('items');
  }

  private goToItemsPage(offset: number): void {
    this.offset = offset;
    this.selected = new Set();
    this.load('items');
  }

  private goToDeletionsPage(offset: number): void {
    this.batchOffset = offset;
    this.load('deletions');
  }

  private selectTab(tab: Tab): void {
    if (this.tab === tab) return;
    this.tab = tab;
    this.ensureLoaded(tab);
  }

  private onTabKey(event: KeyboardEvent, index: number): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const last = TABS.length - 1;
    const destination: Record<string, number> = {
      ArrowRight: index === last ? 0 : index + 1,
      ArrowLeft: index === 0 ? last : index - 1,
      Home: 0,
      End: last,
    };
    const target = TABS[destination[event.key] ?? -1];
    if (!target) return;
    event.preventDefault();
    this.selectTab(target.id);
    this.querySelector<HTMLElement>(`#${tabId(target.id)}`)?.focus();
  }

  // ---- selection -------------------------------------------------------------------------------

  private canRestore(item: TrashItemDto): boolean {
    const volume = this.summary?.volumes.find(
      (candidate) => candidate.volume_id === item.volume_id,
    );
    return blockedReason(volume, 'restore') === null;
  }

  private toggle({ id, selected }: TrashSelectDetail): void {
    const next = new Set(this.selected);
    if (selected) next.add(id);
    else next.delete(id);
    this.selected = next;
  }

  /** The header checkbox: every item of the page that can be restored, or none. */
  private selectPage(selected: boolean): void {
    const next = new Set(this.selected);
    for (const item of this.list?.items ?? []) {
      if (!selected) next.delete(item.id);
      else if (this.canRestore(item)) next.add(item.id);
    }
    this.selected = next;
  }

  // ---- actions ---------------------------------------------------------------------------------

  /** Runs one action at a time: its buttons are disabled until it is over. */
  private async act(action: () => Promise<void>, fallback: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      await action();
    } catch (error) {
      toast.error(errorMessage(error, fallback));
    } finally {
      this.busy = false;
    }
  }

  private restoreBatch(batch: TrashBatchDto): Promise<void> {
    return this.act(async () => {
      const policy = this.summary?.conflict_policy ?? 'smart';
      if (!(await confirmDialog(restoreBatchConfirmation(batch, policy)))) return;
      const task = await restoreTrash({
        target: { kind: 'batch', volume_id: batch.volume_id, batch: batch.batch },
        conflict_policy: null,
      });
      this.track(task);
    }, 'Could not restore the items');
  }

  private restoreItems(ids: readonly number[]): Promise<void> {
    if (ids.length === 0) return Promise.resolve();
    return this.act(async () => {
      const task = await restoreTrash({
        target: { kind: 'ids', ids: [...ids] },
        conflict_policy: null,
      });
      const next = new Set(this.selected);
      for (const id of ids) next.delete(id);
      this.selected = next;
      this.track(task);
    }, 'Could not restore the items');
  }

  /**
   * The confirmation and the request of "Empty trash…". What the question says and what is sent
   * are both taken before the dialog opens: the summary may change while the user reads.
   */
  private confirmEmpty(): Promise<void> {
    const { summary } = this;
    if (this.busy || summary === null) return Promise.resolve();
    const scope = emptyScope(summary, this.volumeId);
    if (scope.deletable === 0) return Promise.resolve();
    const request: TrashEmptyRequest = { volume_id: scope.volumeId, up_to_id: summary.max_id };
    return this.act(async () => {
      if (!(await confirmDialog({ ...emptyConfirmation(scope), danger: true }))) return;
      try {
        this.track(await emptyTrash(request));
      } catch (error) {
        if (!isApiError(error) || error.code !== 'trash_already_empty') throw error;
        toast.info('The trash was already empty');
        this.refresh();
      }
    }, 'Could not empty the trash');
  }

  private lookForUntracked(): Promise<void> {
    return this.act(async () => this.track(await syncTrash()), 'Could not check the trash');
  }

  // ---- tasks -----------------------------------------------------------------------------------

  /** Toasts the start of a task the user just began, or its outcome when it is already over. */
  private track(task: TaskDto): void {
    const ended = isEnded(task) ? task : this.seen.get(task.id);
    if (ended) {
      this.seen.delete(task.id);
      this.announce(ended);
      return;
    }
    toast.info(task.title);
    this.started.add(task.id);
  }

  private announce(task: TaskDto): void {
    const { kind, message } = taskOutcome(task);
    toast[kind](message);
  }

  private remember(task: TaskDto): void {
    this.seen.delete(task.id);
    this.seen.set(task.id, task);
    const oldest = this.seen.keys().next();
    if (this.seen.size > SEEN_LIMIT && !oldest.done) this.seen.delete(oldest.value);
  }

  private readonly onTask = ({ task }: ServerEventOf<'task'>): void => {
    if (!isEnded(task)) return;
    const changesTrash = REFRESH_KINDS.has(task.kind);
    if (this.started.delete(task.id)) this.announce(task);
    else if (changesTrash) this.remember(task);
    if (changesTrash) this.scheduleRefresh();
  };

  private readonly scheduleRefresh = (): void => {
    if (this.refreshTimer !== undefined) return;
    this.refreshTimer = window.setTimeout(() => {
      this.refreshTimer = undefined;
      this.refresh();
    }, REFRESH_DEBOUNCE_MS);
  };

  // ---- rendering -------------------------------------------------------------------------------

  private get emptyHint(): string {
    return retentionText(this.summary?.retention_days ?? null);
  }

  private renderStats() {
    const { summary } = this;
    return html`
      <div class="stats border border-base-300">
        <div class="stat px-4 py-2">
          <div class="stat-title">Items</div>
          <div class="stat-value text-2xl">${summary ? formatCount(summary.items) : EM_DASH}</div>
        </div>
        <div class="stat px-4 py-2">
          <div class="stat-title">Total size</div>
          <div class="stat-value text-2xl">${summary ? formatFileSize(summary.bytes) : EM_DASH}</div>
        </div>
      </div>
    `;
  }

  private renderEmptyButton() {
    const { summary } = this;
    const scope = summary === null ? null : emptyScope(summary, this.volumeId);
    const reason = this.busy ? null : emptyBlockedReason(scope);
    const label =
      scope === null || scope.volumeId === null
        ? `Empty trash${ELLIPSIS}`
        : `Empty trash on ${scope.where}${ELLIPSIS}`;
    return html`
      <span class="inline-block" title=${ifDefined(reason ?? undefined)}>
        <button
          type="button"
          class="btn btn-error btn-outline btn-sm"
          title=${ifDefined(reason ?? undefined)}
          ?disabled=${reason !== null || this.busy}
          @click=${() => this.confirmEmpty()}
        >
          <apm-icon name="trash-2" size="sm"></apm-icon>
          ${label}
        </button>
      </span>
    `;
  }

  private renderMenu() {
    return html`
      <div class="dropdown dropdown-end" @keydown=${closeOnEscape}>
        <div
          tabindex="0"
          role="button"
          class="btn btn-ghost btn-square btn-sm"
          aria-label="More actions"
          title="More actions"
        >
          <apm-icon name="ellipsis-vertical" size="sm"></apm-icon>
        </div>
        <ul
          tabindex="0"
          class="dropdown-content menu z-10 mt-1 w-64 rounded-box border border-base-300 bg-base-100 p-2 shadow-lg"
        >
          <li>
            <button
              type="button"
              title="Add what is in the trash folders on disk but not in this list, and drop what is gone"
              ?disabled=${this.busy}
              @click=${() => {
                closeMenu();
                void this.lookForUntracked();
              }}
            >
              <apm-icon name="search" size="sm"></apm-icon>
              Look for untracked items
            </button>
          </li>
        </ul>
      </div>
    `;
  }

  private renderHeader() {
    const { summary } = this;
    const open = this.tab;
    return html`
      <header class="flex-none border-b border-base-300 px-4 pt-4 sm:px-6">
        <div class="mx-auto w-full max-w-6xl">
          <div class="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
            <div class="flex min-w-0 flex-col gap-2">
              <h2 class="text-xl font-semibold">Trash</h2>
              <div class="flex flex-wrap items-center gap-3">
                ${this.renderStats()} ${summary ? renderVolumeBadges(summary.volumes) : nothing}
              </div>
            </div>
            <div class="flex items-center gap-2">${this.renderEmptyButton()} ${this.renderMenu()}</div>
          </div>
          ${
            summary
              ? html`
                  <div class="alert alert-info alert-soft mt-3 py-2 text-sm">
                    <apm-icon name="info" size="sm"></apm-icon>
                    <span>
                      ${retentionText(summary.retention_days)}
                      <a class="link" href=${linkTo('/settings?section=general')}>
                        Change in Settings
                      </a>
                    </span>
                  </div>
                `
              : nothing
          }
          <div role="tablist" class="tabs tabs-border mt-3" aria-label="Trash views">
            ${TABS.map(
              (tab, index) => html`
                <button
                  type="button"
                  role="tab"
                  id=${tabId(tab.id)}
                  class=${tab.id === open ? 'tab tab-active gap-2' : 'tab gap-2'}
                  aria-selected=${tab.id === open ? 'true' : 'false'}
                  aria-controls=${tab.id === open ? panelId(tab.id) : nothing}
                  tabindex=${tab.id === open ? '0' : '-1'}
                  @click=${() => this.selectTab(tab.id)}
                  @keydown=${(event: KeyboardEvent) => this.onTabKey(event, index)}
                >
                  <apm-icon name=${tab.icon} size="sm"></apm-icon>
                  ${tab.label}
                </button>
              `,
            )}
          </div>
        </div>
      </header>
    `;
  }

  private renderError(title: string, tab: Tab) {
    const { error } = this.status[tab];
    if (error === null) return nothing;
    return html`
      <div role="alert" class="alert alert-error alert-soft my-3">
        <apm-icon name="circle-alert"></apm-icon>
        <div>
          <p class="font-medium">${title}</p>
          <p class="text-sm">${error}</p>
        </div>
        <button type="button" class="btn btn-sm" @click=${() => this.load(tab)}>Retry</button>
      </div>
    `;
  }

  private renderDeletions() {
    const data = this.batches;
    const volumes = this.summary?.volumes ?? [];
    let body: TemplateResult | typeof nothing = nothing;
    if (data === null) {
      body = this.status.deletions.error === null ? renderSkeleton('Loading deletions') : nothing;
    } else if (data.batches.length === 0) {
      body = renderEmpty(this.volumeId !== null, () => this.clearFilters(), this.emptyHint);
    } else {
      body = html`
        <apm-trash-deletions
          .batches=${data.batches}
          .volumes=${volumes}
          .busy=${this.busy}
          @trash-restore-batch=${(event: TrashBatchEvent) => this.restoreBatch(event.detail.batch)}
          @trash-view-batch=${(event: TrashBatchEvent) => this.viewBatch(event.detail.batch)}
        ></apm-trash-deletions>
        ${renderPager(
          {
            offset: this.batchOffset,
            limit: PAGE_SIZE,
            total: data.total,
            count: data.batches.length,
          },
          (offset) => this.goToDeletionsPage(offset),
        )}
      `;
    }
    return html`
      <div class="flex flex-wrap items-center gap-2 py-3">
        ${renderVolumeSelect(volumes, this.volumeId, (volumeId) => this.setVolume(volumeId))}
      </div>
      ${this.renderError('Could not load the deletions', 'deletions')} ${body}
    `;
  }

  private renderItems() {
    const data = this.list;
    if (data === null && this.status.items.error !== null) {
      return this.renderError('Could not load the items', 'items');
    }
    return html`
      ${this.renderError('Could not load the items', 'items')}
      <apm-trash-items
        .items=${data?.items ?? []}
        .volumes=${this.summary?.volumes ?? []}
        .selected=${this.selected}
        .sort=${this.sort}
        .order=${this.effectiveOrder}
        .offset=${this.offset}
        .limit=${PAGE_SIZE}
        .total=${data?.total ?? 0}
        .search=${this.searchDraft}
        .volumeId=${this.volumeId}
        .batchLabel=${this.batchFilter?.label ?? null}
        .filtered=${this.volumeId !== null || this.batchFilter !== null || this.q !== ''}
        .emptyHint=${this.emptyHint}
        .loading=${data === null}
        .busy=${this.busy}
        @trash-search=${(event: CustomEvent<TrashSearchDetail>) => this.onSearch(event.detail.value)}
        @trash-volume=${(event: CustomEvent<TrashVolumeDetail>) =>
          this.setVolume(event.detail.volumeId)}
        @trash-clear-batch=${() => this.clearBatch()}
        @trash-clear-filters=${() => this.clearFilters()}
        @trash-sort=${(event: CustomEvent<TrashSortDetail>) => this.onSort(event.detail)}
        @trash-select=${(event: CustomEvent<TrashSelectDetail>) => this.toggle(event.detail)}
        @trash-select-page=${(event: CustomEvent<TrashSelectPageDetail>) =>
          this.selectPage(event.detail.selected)}
        @trash-restore-items=${(event: CustomEvent<TrashIdsDetail>) =>
          this.restoreItems(event.detail.ids)}
        @trash-page=${(event: CustomEvent<TrashPageDetail>) =>
          this.goToItemsPage(event.detail.offset)}
      ></apm-trash-items>
    `;
  }

  protected override render() {
    const open = this.tab;
    return html`
      <div class="flex h-full min-h-0 flex-col">
        ${this.renderHeader()}
        <div class="min-h-0 flex-1 overflow-y-auto">
          <div class="mx-auto w-full max-w-6xl px-4 pb-4 sm:px-6">
            <div role="tabpanel" id=${panelId(open)} aria-labelledby=${tabId(open)}>
              ${open === 'deletions' ? this.renderDeletions() : this.renderItems()}
            </div>
          </div>
        </div>
      </div>
    `;
  }
}

customElements.define('apm-trash-page', ApmTrashPage);

declare global {
  interface HTMLElementTagNameMap {
    'apm-trash-page': ApmTrashPage;
  }
}
