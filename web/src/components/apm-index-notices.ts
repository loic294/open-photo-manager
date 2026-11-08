import { html, nothing } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import { errorMessage } from '@/api/errors';
import type { HeldRemovalsDto } from '@/api/generated/HeldRemovalsDto';
import { rescan } from '@/api/indexing';
import { ApmElement } from '@/base/apm-element';
import { confirmDialog } from '@/components/confirm-dialog';
import { toast } from '@/components/toast';
import { indexStatus } from '@/stores/index-status';
import { dismissedHostWarnings } from '@/stores/prefs';
import { formatCount } from '@/util/format-count';
import '@/components/apm-icon';

type Action = 'rescan' | 'remove';

interface Pending {
  volumeId: number;
  action: Action;
}

const files = (count: number) => `${formatCount(count)} ${count === 1 ? 'file' : 'files'}`;

/** What the scanner found, in the words of the notice; the numbers drive the grammar. */
function explanation(count: number): string {
  const [entries, them] = count === 1 ? ['its entry was', 'it'] : ['their entries were', 'them'];
  return (
    `${files(count)} could not be found on disk, so ${entries} kept in the index. ` +
    `If the share is mounted correctly you can remove ${them} from the index.`
  );
}

/**
 * The scanner's safety guard: when a scan finds a large share of a volume's indexed files missing
 * (typically an unmounted or half-mounted share) it keeps their entries instead of deleting them
 * from the index. This shows one warning per such volume, with the two ways out: "Rescan" looks
 * again (once the share is back the guard clears by itself) and "Remove from the index" confirms
 * that the files are really gone (after the user agreed in a dialog that names the volume and the
 * count). No file on disk is ever touched. Host warnings are independently dismissible and
 * reappear when the server stops reporting them and later reports them again.
 *
 * The shell mounts one instance, between the connection banner and the page; the data comes from
 * `stores/index-status`, which refreshes whenever an index task ends.
 */
export class ApmIndexNotices extends ApmElement {
  static override properties = {
    pending: { state: true },
  };

  /** The request in progress (its confirmation included): every button waits for it. */
  declare pending: Pending | undefined;

  override connectedCallback() {
    super.connectedCallback();
    this.classList.add('block', 'flex-none');
  }

  protected override willUpdate() {
    const activeCodes = new Set(indexStatus.value.warnings.map((warning) => warning.code));
    const currentCodes = dismissedHostWarnings.peek();
    const nextCodes = Object.fromEntries(
      Object.entries(currentCodes).filter(
        ([code, dismissed]) => dismissed && activeCodes.has(code),
      ),
    );
    if (JSON.stringify(currentCodes) !== JSON.stringify(nextCodes)) {
      dismissedHostWarnings.value = nextCodes;
    }
  }

  private dismissWarning(code: string): void {
    dismissedHostWarnings.value = { ...dismissedHostWarnings.peek(), [code]: true };
  }

  private async run(entry: HeldRemovalsDto, action: Action) {
    if (this.pending) return;
    this.pending = { volumeId: entry.volume_id, action };
    try {
      if (action === 'remove' && !(await this.confirmRemoval(entry))) return;
      await rescan({
        targets: [{ volume_id: entry.volume_id, dir: '', recursive: true }],
        force: false,
        confirm_removals: action === 'remove',
      });
      toast.success(
        action === 'remove'
          ? `Removing the missing files of ${entry.volume_name} from the index`
          : `Rescanning ${entry.volume_name}`,
      );
    } catch (error) {
      toast.error(errorMessage(error, 'Could not start the scan'));
    } finally {
      this.pending = undefined;
    }
  }

  private confirmRemoval({ volume_name: name, files: count }: HeldRemovalsDto): Promise<boolean> {
    return confirmDialog({
      title: `Remove ${files(count)} from the index?`,
      message:
        `${files(count)} of volume ${name} could not be found on disk. ` +
        'Removing them drops their entries from the index; no file on disk is touched. ' +
        'If the volume is only mounted wrongly, cancel, fix the mount and rescan instead.',
      confirmLabel: 'Remove from the index',
      danger: true,
    });
  }

  private renderButton(entry: HeldRemovalsDto, action: Action, label: string) {
    const { pending } = this;
    const spinning = pending?.volumeId === entry.volume_id && pending.action === action;
    return html`
      <button
        type="button"
        class="btn btn-sm"
        ?disabled=${pending !== undefined}
        @click=${() => void this.run(entry, action)}
      >
        ${spinning ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}
        ${label}
      </button>
    `;
  }

  private renderNotice(entry: HeldRemovalsDto) {
    return html`
      <div
        role="alert"
        class="alert alert-warning alert-vertical sm:alert-horizontal flex-none rounded-none"
      >
        <apm-icon name="triangle-alert"></apm-icon>
        <span><strong>Volume ${entry.volume_name}:</strong> ${explanation(entry.files)}</span>
        <div class="flex flex-wrap gap-2" role="group" aria-label="Volume ${entry.volume_name}">
          ${this.renderButton(entry, 'rescan', 'Rescan')}
          ${this.renderButton(entry, 'remove', 'Remove from the index')}
        </div>
      </div>
    `;
  }

  private renderHostWarning(warning: { code: string; message: string }) {
    return html`
      <div
        role="alert"
        class="alert alert-info alert-vertical sm:alert-horizontal flex-none rounded-none"
        data-warning-code=${warning.code}
      >
        <apm-icon name="info"></apm-icon>
        <span>${warning.message}</span>
        <button
          type="button"
          class="btn btn-sm"
          aria-label="Dismiss warning"
          @click=${() => this.dismissWarning(warning.code)}
        >
          Dismiss
        </button>
      </div>
    `;
  }

  protected override render() {
    const { held_removals: held, warnings } = indexStatus.value;
    const dismissed = dismissedHostWarnings.value;
    return html`
      ${repeat(
        held,
        (entry) => entry.volume_id,
        (entry) => this.renderNotice(entry),
      )}
      ${repeat(
        warnings.filter((warning) => !dismissed[warning.code]),
        (warning) => warning.code,
        (warning) => this.renderHostWarning(warning),
      )}
    `;
  }
}

customElements.define('apm-index-notices', ApmIndexNotices);

declare global {
  interface HTMLElementTagNameMap {
    'apm-index-notices': ApmIndexNotices;
  }
}
