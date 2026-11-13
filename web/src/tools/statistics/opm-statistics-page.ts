import { html, nothing, svg } from 'lit';
import { errorMessage, isAbortError } from '@/api/errors';
import type { StatisticsResponse } from '@/api/generated/StatisticsResponse';
import { events } from '@/api/sse';
import { getStatistics } from '@/api/statistics';
import { OpmElement } from '@/base/opm-element';
import { linkTo } from '@/stores/nav';
import { isEnded } from '@/stores/tasks';
import { volumes } from '@/stores/volumes';
import {
  ACTIVITIES,
  type Activity,
  type Day,
  dailyRows,
  latestInventory,
  localDay,
} from '@/tools/statistics/statistics-data';
import { EM_DASH, formatFileSize, formatFileTime } from '@/util/format';
import { formatCount } from '@/util/format-count';

const count = (value: number | null) => (value === null ? EM_DASH : formatCount(value));
const bytes = (value: number | null) => (value === null ? EM_DASH : formatFileSize(value));
const SECTION = 'flex min-w-0 flex-col gap-3';
const TABLE = 'table table-sm table-zebra';

export class OpmStatisticsPage extends OpmElement {
  static override properties = {
    data: { state: true },
    loading: { state: true },
    problem: { state: true },
    volume: { state: true },
    range: { state: true },
    from: { state: true },
    to: { state: true },
    activity: { state: true },
  };

  declare private data: StatisticsResponse | null;
  loading = false;
  problem = '';
  volume = '';
  range = '30';
  from = '';
  to = '';
  activity: Activity = 'added';
  private request?: AbortController;
  private dispose: (() => void)[] = [];
  private timer?: number;

  constructor() {
    super();
    this.data = null;
  }

  override connectedCallback() {
    super.connectedCallback();
    window.addEventListener('focus', this.refresh);
    this.dispose = [
      events.onResync(this.refresh),
      events.on('task', ({ task }) => {
        if (isEnded(task)) this.schedule();
      }),
    ];
    this.refresh();
  }

  override disconnectedCallback() {
    this.request?.abort();
    window.clearTimeout(this.timer);
    window.removeEventListener('focus', this.refresh);
    for (const dispose of this.dispose) dispose();
    super.disconnectedCallback();
  }

  private schedule() {
    if (this.timer !== undefined) return;
    this.timer = window.setTimeout(() => {
      this.timer = undefined;
      this.refresh();
    }, 1000);
  }

  private readonly refresh = () => {
    void this.load();
  };

  private async load() {
    if (this.range === 'custom' && (!this.from || !this.to)) return;
    this.request?.abort();
    const request = new AbortController();
    this.request = request;
    this.loading = true;
    this.problem = '';
    try {
      const data = await getStatistics(
        {
          from: this.from || null,
          to: this.to || null,
          volume_id: this.volume === '' ? null : Number(this.volume),
        },
        request.signal,
      );
      if (request.signal.aborted) return;
      this.data = data;
    } catch (error) {
      if (!request.signal.aborted && !isAbortError(error)) {
        this.problem = errorMessage(error, 'Could not load library statistics');
      }
    } finally {
      if (!request.signal.aborted) this.loading = false;
    }
  }

  private setRange(value: string) {
    this.range = value;
    if (value === 'custom') {
      this.from = this.data?.from ?? '';
      this.to = this.data?.to ?? '';
      return;
    }
    const end = localDay(Date.now() / 1000, this.data?.timezone ?? 'America/Los_Angeles');
    const start = new Date(`${end}T00:00:00Z`);
    start.setUTCDate(start.getUTCDate() - Number(value) + 1);
    this.from = value === '30' ? '' : start.toISOString().slice(0, 10);
    this.to = '';
    this.refresh();
  }

  protected override render() {
    const data = this.data;
    const rows = data ? dailyRows(data) : [];
    const inventory = latestInventory(rows);
    const names = new Map(volumes.value.map((volume) => [volume.id, volume.name]));
    for (const row of data?.snapshots ?? []) {
      names.set(row.inventory.volume_id, row.inventory.volume_name);
    }
    return html`
      <div class="h-full overflow-auto p-4 md:p-6" aria-busy=${this.loading ? 'true' : 'false'}>
        <div class="mx-auto flex max-w-7xl flex-col gap-8">
          <div class="flex flex-wrap items-end gap-3">
            <label class="fieldset">
              <span class="fieldset-legend">Volume</span>
              <select class="select" aria-label="Statistics volume" .value=${this.volume}
                @change=${(event: Event) => {
                  this.volume = (event.target as HTMLSelectElement).value;
                  this.refresh();
                }}>
                <option value="">All volumes</option>
                ${[...names].map(([id, name]) => html`<option value=${id}>${name}</option>`)}
              </select>
            </label>
            <label class="fieldset">
              <span class="fieldset-legend">Period</span>
              <select class="select" aria-label="Statistics period" .value=${this.range}
                @change=${(event: Event) => this.setRange((event.target as HTMLSelectElement).value)}>
                <option value="30">Last 30 days</option>
                <option value="90">Last 90 days</option>
                <option value="365">Last year</option>
                <option value="custom">Custom dates</option>
              </select>
            </label>
            ${
              this.range === 'custom'
                ? html`
              <label class="fieldset"><span class="fieldset-legend">From</span>
                <input type="date" class="input" .value=${this.from}
                  @change=${(event: Event) => {
                    this.from = (event.target as HTMLInputElement).value;
                  }}>
              </label>
              <label class="fieldset"><span class="fieldset-legend">To</span>
                <input type="date" class="input" .value=${this.to}
                  @change=${(event: Event) => {
                    this.to = (event.target as HTMLInputElement).value;
                  }}>
              </label>
              <button class="btn" ?disabled=${this.loading || !this.from || !this.to}
                @click=${this.refresh}>Apply dates</button>
            `
                : nothing
            }
            <button class="btn btn-ghost" ?disabled=${this.loading} @click=${this.refresh}>
              ${this.loading ? 'Refreshing…' : 'Refresh'}
            </button>
          </div>
          ${
            this.problem
              ? html`
            <div class="alert alert-error alert-soft" role="alert">
              <span>${this.problem}</span><button class="btn btn-sm" @click=${this.refresh}>Retry</button>
            </div>
          `
              : nothing
          }
          ${
            data
              ? html`
            <div class="flex flex-wrap items-center justify-between gap-2 text-sm">
              <p>Collection updated ${formatFileTime(data.observed_at, undefined, data.timezone)} · ${data.timezone}</p>
              <a class="link" href=${linkTo('/settings?section=general')}>Reporting timezone settings</a>
            </div>
            <div class="alert alert-info alert-soft text-sm" role="note">
              <p class="text-base-content">History starts ${formatFileTime(data.started_at, undefined, data.timezone)}. Today is provisional.
                Inventory is the latest recorded observation, not an exact midnight count.
                Gaps and partial days are not zero activity.
                ${data.pending_events ? `${count(data.pending_events)} changes are still being processed.` : ''}
              </p>
            </div>
            ${
              inventory.some((volume) => !volume.baseline_complete)
                ? html`
              <div class="alert alert-warning alert-soft text-base-content" role="status">
                Initial inventory is still being established. These discoveries are not counted as new additions.
              </div>
            `
                : nothing
            }
            ${
              inventory.length === 0
                ? html`
              <div class="py-8 text-center">
                <h2 class="text-lg font-semibold">No inventory observations in this period</h2>
                <p class="mt-2 text-sm">Choose another period or index a volume. Earlier history cannot be reconstructed.</p>
              </div>
            `
                : html`
              <section class=${SECTION} aria-labelledby="statistics-inventory">
                <div>
                  <h2 id="statistics-inventory" class="text-lg font-semibold">Library over time</h2>
                  <p class="text-sm">All indexed files, including sidecars and retained offline volumes. Trash and plan ghosts are excluded.</p>
                </div>
                <div class="grid min-w-0 gap-6 md:grid-cols-2">
                  ${this.chart(rows, 'files', 'Indexed files', count)}
                  ${this.chart(rows, 'bytes', 'Indexed logical size', bytes)}
                </div>
              </section>
            `
            }
            <section class=${SECTION} aria-labelledby="statistics-activity">
              <div class="flex flex-wrap items-end justify-between gap-3">
                <div><h2 id="statistics-activity" class="text-lg font-semibold">Daily activity</h2>
                  <p class="text-sm">Distinct files per day. Moves, unchanged rescans, and metadata processing are excluded.</p></div>
                <label class="fieldset"><span class="fieldset-legend">Activity chart</span>
                  <select class="select" aria-label="Activity chart" .value=${this.activity}
                    @change=${(event: Event) => {
                      const value = (event.target as HTMLSelectElement).value;
                      const selected = ACTIVITIES.find(([key]) => key === value);
                      if (selected) this.activity = selected[0];
                    }}>
                    ${ACTIVITIES.map(([key, label]) => html`<option value=${key}>${label}</option>`)}
                  </select>
                </label>
              </div>
              ${this.chart(rows, this.activity, ACTIVITIES.find(([key]) => key === this.activity)?.[1] ?? '', count, true)}
              <p class="text-sm">Duplicate cleanup moves files to trash; those bytes are not freed space.
                “Permanently deleted” reports logical bytes purged, not measured physical disk space.</p>
            </section>
            ${this.details(rows)}
            ${this.dailyTable(rows)}
          `
              : this.loading
                ? html`
            <div role="status" aria-label="Loading statistics" class="flex flex-col gap-4">
              <div class="skeleton h-64"></div><div class="skeleton h-64"></div>
            </div>
          `
                : nothing
          }
        </div>
      </div>
    `;
  }

  private chart(
    rows: Day[],
    key: string,
    label: string,
    format: (value: number | null) => string,
    bars = false,
  ) {
    const values = rows.map((row) =>
      bars
        ? row.state === 'No observation'
          ? null
          : (row.activity[key]?.count ?? 0)
        : key === 'files'
          ? row.files
          : row.bytes,
    );
    const max = Math.max(1, ...values.filter((value) => value !== null));
    const x = (index: number) => 12 + ((index + 0.5) * 616) / Math.max(1, rows.length);
    const y = (value: number) => 164 - (value / max) * 144;
    let path = '';
    values.forEach((value, index) => {
      if (value === null) return;
      const connected =
        index > 0 &&
        values[index - 1] !== null &&
        rows[index - 1]?.segment === rows[index]?.segment;
      path += `${connected ? 'L' : 'M'}${x(index)},${y(value)} `;
    });
    return html`
      <figure class="min-w-0 rounded-box border border-base-300 p-4">
        <figcaption class="flex justify-between gap-2 text-sm font-medium">
          <span>${label}</span><span class="text-base-content/70">Scale: 0–${format(max)}</span>
        </figcaption>
        <svg viewBox="0 0 640 188" class="mt-3 block w-full text-base-content" role="img"
          aria-label=${`${label} from ${rows[0]?.date ?? ''} to ${rows.at(-1)?.date ?? ''}. Exact values and missing observations are in the Daily details table.`}>
          <line x1="12" x2="628" y1="164" y2="164" class="stroke-base-300"></line>
          ${
            bars
              ? values.map((value, index) =>
                  value === null
                    ? nothing
                    : svg`
            <rect x=${x(index) - Math.max(1, 240 / Math.max(1, rows.length))}
              y=${y(value)} width=${Math.max(1, 480 / Math.max(1, rows.length))}
              height=${Math.max(value > 0 ? 1 : 0, 164 - y(value))} fill="currentColor"></rect>
          `,
                )
              : svg`
            <path d=${path} fill="none" stroke="currentColor" stroke-width="2"></path>
            ${values.map((value, index) =>
              value === null
                ? nothing
                : svg`
              <circle cx=${x(index)} cy=${y(value)} r="2.5" fill="currentColor"></circle>
            `,
            )}
          `
          }
        </svg>
        <div class="flex justify-between gap-2 text-xs">
          <span>${rows[0]?.date}</span><span>${rows.at(-1)?.date}</span>
        </div>
      </figure>
    `;
  }

  private details(rows: Day[]) {
    const inventory = latestInventory(rows);
    const kinds = new Map<string, { files: number; size: number }>();
    for (const volume of inventory) {
      for (const kind of volume.kinds) {
        const sum = kinds.get(kind.kind) ?? { files: 0, size: 0 };
        sum.files += kind.files;
        sum.size += kind.bytes;
        kinds.set(kind.kind, sum);
      }
    }
    return html`
      <section class=${SECTION} aria-labelledby="statistics-volumes">
        <h2 id="statistics-volumes" class="text-lg font-semibold">Volumes at latest observation</h2>
        <div class="overflow-x-auto" tabindex="0" role="region" aria-label="Volume inventory table">
          <table class=${TABLE}>
            <thead><tr><th>Volume</th><th>Status</th><th>Files</th><th>Logical size</th><th>Trash size</th><th>Last scan</th></tr></thead>
            <tbody>${inventory.map(
              (volume) => html`
              <tr><th>${volume.volume_name}</th><td>${volume.online ? 'Online' : 'Offline · retained index'}</td>
                <td>${count(volume.files)}</td><td>${bytes(volume.bytes)}</td><td>${bytes(volume.trash_bytes)}</td>
                <td>${formatFileTime(volume.last_scan_at, undefined, this.data?.timezone)}</td></tr>
            `,
            )}</tbody>
          </table>
        </div>
      </section>
      <div class="grid min-w-0 gap-8 lg:grid-cols-2">
        <section class=${SECTION} aria-labelledby="statistics-types">
          <h2 id="statistics-types" class="text-lg font-semibold">File types</h2>
          <div class="overflow-x-auto" tabindex="0" role="region" aria-label="File type statistics table"><table class=${TABLE}>
            <thead><tr><th>Type</th><th>Files</th><th>Logical size</th></tr></thead>
            <tbody>${[...kinds].map(
              ([kind, value]) => html`
              <tr><th>${kind}</th><td>${count(value.files)}</td><td>${bytes(value.size)}</td></tr>
            `,
            )}</tbody>
          </table></div>
        </section>
        <section class=${SECTION} aria-labelledby="statistics-health">
          <h2 id="statistics-health" class="text-lg font-semibold">Index health</h2>
          <div class="overflow-x-auto" tabindex="0" role="region" aria-label="Index health table"><table class=${TABLE}>
            <thead><tr><th>Volume</th><th>Stage</th><th>Pending</th><th>Failed</th></tr></thead>
            <tbody>${inventory.flatMap((volume) =>
              [
                ['Metadata', volume.metadata_pending, volume.metadata_failed],
                ['Hashes', volume.hashes_pending, volume.hashes_failed],
                ['Thumbnails', volume.thumbnails_pending, volume.thumbnails_failed],
                ['Held removals', volume.held_removals, 0],
              ].map(
                ([label, pending, failed]) => html`
              <tr><th>${volume.volume_name}</th><td>${label}</td>
                <td>${count(Number(pending))}</td><td>${count(Number(failed))}</td></tr>
            `,
              ),
            )}</tbody>
          </table></div>
        </section>
      </div>
    `;
  }

  private dailyTable(rows: Day[]) {
    return html`
      <section class=${SECTION} aria-labelledby="statistics-daily">
        <h2 id="statistics-daily" class="text-lg font-semibold">Daily details</h2>
        <p class="text-sm">Counts overlap: duplicates are part of trashed files, and trashed files leave the index.
          A file may be both added and modified. ${EM_DASH} means no observation, not zero.</p>
        <div class="overflow-x-auto" tabindex="0" role="region" aria-label="Daily statistics table">
          <table class="${TABLE} whitespace-nowrap">
            <thead><tr><th>Day / timezone</th><th>Coverage</th><th>Files</th><th>Logical size</th><th>Net change</th>
              ${ACTIVITIES.map(([, label]) => html`<th>${label}</th>`)}
              <th>Duplicate bytes trashed</th><th>Bytes purged</th><th>Observed</th></tr></thead>
            <tbody>${rows.toReversed().map(
              (row) => html`
              <tr><th>${row.date}<span class="block text-xs font-normal">${row.timezone} · segment ${row.segment}</span></th>
                <td>${row.state}</td><td>${count(row.files)}</td><td>${row.bytes === null ? EM_DASH : `${count(row.bytes)} B`}</td>
                <td>${row.net === null ? EM_DASH : row.net > 0 ? `+${count(row.net)}` : count(row.net)}</td>
                ${ACTIVITIES.map(
                  ([key]) => html`
                  <td>${row.state === 'No observation' ? EM_DASH : count(row.activity[key]?.count ?? 0)}</td>
                `,
                )}
                <td>${row.state === 'No observation' ? EM_DASH : `${count(row.activity.duplicates?.bytes ?? 0)} B`}</td>
                <td>${row.state === 'No observation' ? EM_DASH : `${count(row.activity.purged?.bytes ?? 0)} B`}</td>
                <td>${formatFileTime(row.observedAt, undefined, row.timezone)}</td>
              </tr>
            `,
            )}</tbody>
          </table>
        </div>
      </section>
    `;
  }
}

customElements.define('opm-statistics-page', OpmStatisticsPage);

declare global {
  interface HTMLElementTagNameMap {
    'opm-statistics-page': OpmStatisticsPage;
  }
}
