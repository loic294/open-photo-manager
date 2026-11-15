import { html } from 'lit';
import { ifDefined } from 'lit/directives/if-defined.js';
import { repeat } from 'lit/directives/repeat.js';
import type { TrashBatchDto } from '@/api/generated/TrashBatchDto';
import type { TrashVolumeSummary } from '@/api/generated/TrashVolumeSummary';
import { OpmElement } from '@/base/opm-element';
import { tooltip } from '@/components/tooltip';
import { renderReasonBadge } from '@/tools/trash/trash-parts';
import { blockedReason, relativeTime } from '@/tools/trash/trash-text';
import { formatFileSize, formatFileTime } from '@/util/format';
import { formatCount } from '@/util/format-count';
import '@/components/opm-icon';

export interface TrashBatchDetail {
  batch: TrashBatchDto;
}

export type TrashBatchEvent = CustomEvent<TrashBatchDetail>;

declare global {
  interface GlobalEventHandlersEventMap {
    'trash-restore-batch': TrashBatchEvent;
    'trash-view-batch': TrashBatchEvent;
  }
}

/** The key of a deletion: batch names repeat across volumes. */
export const batchKey = (batch: Pick<TrashBatchDto, 'volume_id' | 'batch'>) =>
  `${batch.volume_id}/${batch.batch}`;

/**
 * The "Deletions" tab of the Trash page: one row per deletion (one batch folder in the trash),
 * newest first. Presentational: the page owns the data and does the work.
 *
 * Properties: `batches`, `volumes` (the volumes that hold trash: they say which rows cannot be
 * restored now) and `busy` (a request is on its way: "Restore all" waits).
 *
 * Events (bubble, `detail: { batch }`): `trash-restore-batch` ("Restore all") and
 * `trash-view-batch` ("View items": the page shows that deletion on the Items tab).
 */
export class OpmTrashDeletions extends OpmElement {
  static override properties = {
    batches: { attribute: false },
    volumes: { attribute: false },
    busy: { type: Boolean },
  };

  declare batches: readonly TrashBatchDto[];
  declare volumes: readonly TrashVolumeSummary[];
  declare busy: boolean;

  constructor() {
    super();
    this.batches = [];
    this.volumes = [];
    this.busy = false;
  }

  private emit(name: 'trash-restore-batch' | 'trash-view-batch', batch: TrashBatchDto): void {
    this.dispatchEvent(
      new CustomEvent<TrashBatchDetail>(name, { detail: { batch }, bubbles: true }),
    );
  }

  private renderRow(batch: TrashBatchDto) {
    const volume = this.volumes.find((candidate) => candidate.volume_id === batch.volume_id);
    const blocked = blockedReason(volume, 'restore');
    return html`
      <tr>
        <td class="whitespace-nowrap">
          <span ${tooltip(formatFileTime(batch.trashed_at))}>${relativeTime(batch.trashed_at)}</span>
        </td>
        <td>${batch.volume}</td>
        <td class="text-right tabular-nums">${formatCount(batch.items)}</td>
        <td class="whitespace-nowrap text-right tabular-nums">${formatFileSize(batch.bytes)}</td>
        <td>
          <div class="flex flex-wrap gap-1">
            ${
              batch.reasons.length === 0
                ? renderReasonBadge(null)
                : batch.reasons.map((reason) => renderReasonBadge(reason))
            }
          </div>
        </td>
        <td class="whitespace-nowrap text-right">
          <span class="inline-block" ${tooltip(ifDefined(blocked ?? undefined))}>
            <button
              type="button"
              class="btn btn-sm btn-ghost"
              ${tooltip(ifDefined(blocked ?? undefined))}
              ?disabled=${blocked !== null || this.busy}
              @click=${() => this.emit('trash-restore-batch', batch)}
            >
              <opm-icon name="undo-2" size="sm"></opm-icon>
              Restore all
            </button>
          </span>
          <button
            type="button"
            class="btn btn-sm btn-ghost"
            @click=${() => this.emit('trash-view-batch', batch)}
          >
            <opm-icon name="list" size="sm"></opm-icon>
            View items
          </button>
        </td>
      </tr>
    `;
  }

  protected override render() {
    return html`
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <caption class="sr-only">
            Deletions, newest first
          </caption>
          <thead>
            <tr>
              <th scope="col">Deleted</th>
              <th scope="col">Volume</th>
              <th scope="col" class="text-right">Items</th>
              <th scope="col" class="text-right">Size</th>
              <th scope="col">Reason</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            ${repeat(this.batches, batchKey, (batch) => this.renderRow(batch))}
          </tbody>
        </table>
      </div>
    `;
  }
}

customElements.define('opm-trash-deletions', OpmTrashDeletions);

declare global {
  interface HTMLElementTagNameMap {
    'opm-trash-deletions': OpmTrashDeletions;
  }
}
