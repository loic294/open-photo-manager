import { html, nothing } from 'lit';
import { errorMessage } from '@/api/errors';
import type { IsmBindingDto } from '@/api/generated/IsmBindingDto';
import type { IsmTrashDto } from '@/api/generated/IsmTrashDto';
import { fetchIsmStatus, listIsmTrash, purgeIsmTrash, restoreIsmTrash } from '@/api/ism';
import { events, type ServerEventOf } from '@/api/sse';
import { OpmElement } from '@/base/opm-element';
import { confirmDialog } from '@/components/confirm-dialog';
import { toast } from '@/components/toast';
import { isEnded, isFailure } from '@/stores/tasks';
import { formatFileSize } from '@/util/format';

/** What one bound volume shows: its ISM trash, or why it could not be read. */
interface Section {
  binding: IsmBindingDto;
  trash: IsmTrashDto | null;
  error: string | null;
}

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`;

/**
 * The trash of the sync manager (ISM) for each volume bound to it. OPM never trashes files on
 * those volumes itself: ISM keeps them, and restore or purge are requests to ISM run as tasks.
 * Purge is shown only when the token may purge, and is always confirmed. Renders nothing when
 * no volume is bound.
 */
export class OpmIsmTrash extends OpmElement {
  static override properties = {
    sections: { state: true },
    selected: { state: true },
    busy: { state: true },
  };

  declare sections: Section[];
  /** Selected trash ids, per volume id. */
  declare selected: ReadonlyMap<number, ReadonlySet<string>>;
  declare busy: boolean;

  /** Tasks this view started and has not seen end. */
  private readonly started = new Set<number>();
  private unsubscribe: (() => void)[] = [];
  private loading: AbortController | null = null;

  constructor() {
    super();
    this.sections = [];
    this.selected = new Map();
    this.busy = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
    this.unsubscribe = [events.on('task', this.onTask), events.onResync(() => void this.load())];
    void this.load();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    for (const off of this.unsubscribe) off();
    this.unsubscribe = [];
    this.loading?.abort();
    this.loading = null;
  }

  async load(): Promise<void> {
    this.loading?.abort();
    const controller = new AbortController();
    this.loading = controller;
    let bindings: IsmBindingDto[];
    try {
      bindings = (await fetchIsmStatus(controller.signal)).bindings ?? [];
    } catch {
      // Without the sync manager status there is nothing to show; the settings page says why.
      if (!controller.signal.aborted) this.sections = [];
      return;
    }
    const sections = await Promise.all(
      bindings.map(async (binding): Promise<Section> => {
        try {
          const trash = await listIsmTrash(binding.volume_id, controller.signal);
          return { binding, trash, error: null };
        } catch (error) {
          return { binding, trash: null, error: errorMessage(error) };
        }
      }),
    );
    if (controller.signal.aborted) return;
    this.sections = sections;
    const present = new Map(
      sections.map((s) => [s.binding.volume_id, new Set(s.trash?.items.map((i) => i.trash_id))]),
    );
    const kept = new Map<number, ReadonlySet<string>>();
    for (const [volume, ids] of this.selected) {
      const still = [...ids].filter((id) => present.get(volume)?.has(id));
      if (still.length > 0) kept.set(volume, new Set(still));
    }
    this.selected = kept;
  }

  private readonly onTask = ({ task }: ServerEventOf<'task'>): void => {
    if (!isEnded(task) || !this.started.delete(task.id)) return;
    if (isFailure(task)) toast.error(task.error ?? `${task.title} failed`);
    else toast.success(`${task.title}: done`);
    void this.load();
  };

  private toggle(volumeId: number, trashId: string, on: boolean): void {
    const next = new Set(this.selected.get(volumeId));
    if (on) next.add(trashId);
    else next.delete(trashId);
    const selected = new Map(this.selected);
    selected.set(volumeId, next);
    this.selected = selected;
  }

  private async restore(section: Section): Promise<void> {
    const ids = [...(this.selected.get(section.binding.volume_id) ?? [])];
    if (ids.length === 0) return;
    await this.start(() =>
      restoreIsmTrash({ volume_id: section.binding.volume_id, trash_ids: ids }),
    );
  }

  private async purge(section: Section): Promise<void> {
    const ids = [...(this.selected.get(section.binding.volume_id) ?? [])];
    if (ids.length === 0) return;
    const confirmed = await confirmDialog({
      title: `Delete ${plural(ids.length, 'item')} for good?`,
      message: `The sync manager deletes ${plural(ids.length, 'item')} from its trash on ${section.binding.root_name}. This cannot be undone.`,
      confirmLabel: `Delete ${plural(ids.length, 'item')} for good`,
      danger: true,
    });
    if (!confirmed) return;
    await this.start(() =>
      purgeIsmTrash({ volume_id: section.binding.volume_id, trash_ids: ids, confirm: true }),
    );
  }

  private async start(request: () => Promise<{ id: number }>): Promise<void> {
    this.busy = true;
    try {
      const task = await request();
      this.started.add(task.id);
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      this.busy = false;
    }
  }

  private renderSection(section: Section) {
    const { binding, trash, error } = section;
    const selected = this.selected.get(binding.volume_id) ?? new Set<string>();
    let body: unknown;
    if (error !== null) {
      body = html`<div role="alert" class="alert alert-error alert-soft">${error}</div>`;
    } else if (trash === null || trash.items.length === 0) {
      body = html`<p class="text-sm text-base-content/70">Nothing in the sync manager trash.</p>`;
    } else {
      body = html`
        <div class="overflow-x-auto">
          <table class="table table-sm">
            <thead>
              <tr><th></th><th>Path</th><th>Size</th><th>Trashed</th><th>Immich assets</th></tr>
            </thead>
            <tbody>
              ${trash.items.map(
                (item) => html`
                  <tr>
                    <td>
                      <input
                        type="checkbox"
                        class="checkbox checkbox-sm"
                        aria-label=${`Select ${item.path}`}
                        .checked=${selected.has(item.trash_id)}
                        @change=${(event: Event) =>
                          this.toggle(
                            binding.volume_id,
                            item.trash_id,
                            (event.target as HTMLInputElement).checked,
                          )}
                      />
                    </td>
                    <td class="break-all">${item.path}${item.is_folder ? '/' : ''}</td>
                    <td>${formatFileSize(item.size)}</td>
                    <td>${new Date(item.trashed_at).toLocaleString()}</td>
                    <td>${item.asset_count}</td>
                  </tr>
                `,
              )}
            </tbody>
          </table>
        </div>
      `;
    }
    const none = selected.size === 0 || this.busy;
    return html`
      <section class="card card-border bg-base-100 my-3" aria-label=${`Sync manager trash of ${binding.root_name}`}>
        <div class="card-body gap-3 p-4">
          <div class="flex flex-wrap items-center gap-2">
            <h3 class="card-title text-base">Sync manager trash: ${binding.root_name}</h3>
            <span class="badge badge-ghost">${trash?.items.length ?? 0}</span>
            <div class="flex-1"></div>
            <button
              type="button"
              class="btn btn-sm"
              ?disabled=${none}
              @click=${() => this.restore(section)}
            >
              Restore
            </button>
            ${
              trash?.can_purge
                ? html`<button
                    type="button"
                    class="btn btn-sm btn-error"
                    ?disabled=${none}
                    @click=${() => this.purge(section)}
                  >
                    Delete for good…
                  </button>`
                : nothing
            }
          </div>
          ${body}
        </div>
      </section>
    `;
  }

  protected override render() {
    if (this.sections.length === 0) return nothing;
    return html`${this.sections.map((section) => this.renderSection(section))}`;
  }
}

customElements.define('opm-ism-trash', OpmIsmTrash);

declare global {
  interface HTMLElementTagNameMap {
    'opm-ism-trash': OpmIsmTrash;
  }
}
