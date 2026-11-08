import { html, nothing } from 'lit';
import { live } from 'lit/directives/live.js';
import { errorMessage, isApiError } from '@/api/errors';
import type { FieldError } from '@/api/generated/FieldError';
import type { SettingsDto } from '@/api/generated/SettingsDto';
import { ApmElement } from '@/base/apm-element';
import { ApmChipInput, type ChipValuesChange } from '@/components/apm-chip-input';
import '@/components/apm-icon';
import '@/components/apm-save-bar';
import '@/components/apm-select';
import { toast } from '@/components/toast';
import { UnsavedGuard } from '@/controllers/unsaved-guard';
import { loadSettings, saveSettings } from '@/stores/settings';
import {
  DEFAULT_TRASH_RETENTION_DAYS,
  ENCRYPTED_SHARE_NAME_LEN,
  HASH_CONCURRENCY,
  isInRange,
  isValidRescanInterval,
  MAX_JUNK_ENTRIES,
  MAX_SIDECAR_EXTENSIONS,
  NAME_MAX_LEN,
  type NumberRange,
  normalizeExtension,
  normalizeJunkEntry,
  RESCAN_INTERVAL_MINUTES,
  RESCAN_NEVER,
  SCAN_CONCURRENCY,
  THUMBNAIL_CONCURRENCY,
  TRASH_RETENTION_DAYS,
  validateExtension,
  validateJunkName,
  validateJunkPrefix,
} from '@/tools/settings/limits';
import { CONFLICT_POLICIES, isConflictPolicy } from '@/util/conflict-policies';

type ListField = 'sidecar_extensions' | 'junk_names' | 'junk_prefixes';
type NumberField =
  | 'name_max_len'
  | 'trash_retention_days'
  | 'index.concurrency'
  | 'index.hash_concurrency'
  | 'index.thumbnail_concurrency'
  | 'index.rescan_interval_minutes';
/** The fields the server can refuse, and so the ones a validation error is mapped onto. */
type FieldId = ListField | NumberField;

const LIST_FIELDS: readonly ListField[] = ['sidecar_extensions', 'junk_names', 'junk_prefixes'];

/** The fields in page order: after a refused save the first one with a problem gets the focus. */
const FIELD_ORDER: readonly FieldId[] = [
  ...LIST_FIELDS,
  'name_max_len',
  'trash_retention_days',
  'index.concurrency',
  'index.hash_concurrency',
  'index.thumbnail_concurrency',
  'index.rescan_interval_minutes',
];

interface ListSpec {
  label: string;
  placeholder: string;
  hint: string;
  max: number;
  /** Entries are file names, which may contain spaces. */
  allowSpaces: boolean;
  normalize: (raw: string) => string;
  validate: (entry: string) => string | null;
}

const LISTS: Record<ListField, ListSpec> = {
  sidecar_extensions: {
    label: 'Sidecar extensions',
    placeholder: 'xmp',
    hint: 'Companion files that travel with a photo, for example IMG_1.xmp. Type them without the dot.',
    max: MAX_SIDECAR_EXTENSIONS,
    allowSpaces: false,
    normalize: normalizeExtension,
    validate: validateExtension,
  },
  junk_names: {
    label: 'Ignored names',
    placeholder: 'Thumbs.db',
    hint: 'Files and folders with these names are never indexed and do not count as content when checking whether a folder is empty. Case does not matter.',
    max: MAX_JUNK_ENTRIES,
    allowSpaces: true,
    normalize: normalizeJunkEntry,
    validate: validateJunkName,
  },
  junk_prefixes: {
    label: 'Ignored prefixes',
    placeholder: '._',
    hint: 'The same for names that start with this text, for example ._ for macOS resource forks.',
    max: MAX_JUNK_ENTRIES,
    allowSpaces: true,
    normalize: normalizeJunkEntry,
    validate: validateJunkPrefix,
  },
};

interface NumberRule {
  /** The value in the document; `null` when the field does not apply (the retention while off). */
  read: (doc: SettingsDto) => number | null;
  write: (doc: SettingsDto, value: number) => SettingsDto;
  /** Bounds of the `<input>`: they only steer its arrows, validity is decided by `valid`. */
  min: number;
  max: number;
  valid: (value: number) => boolean;
  /** What to tell the user while `valid` fails. */
  message: string;
}

function inRange(
  range: NumberRange,
  read: NumberRule['read'],
  write: NumberRule['write'],
): NumberRule {
  return {
    read,
    write,
    min: range.min,
    max: range.max,
    valid: (value) => isInRange(value, range),
    message: `Enter a whole number from ${range.min} to ${range.max}.`,
  };
}

const NUMBER_RULES: Record<NumberField, NumberRule> = {
  name_max_len: inRange(
    NAME_MAX_LEN,
    (doc) => doc.name_max_len,
    (doc, value) => ({ ...doc, name_max_len: value }),
  ),
  trash_retention_days: inRange(
    TRASH_RETENTION_DAYS,
    (doc) => doc.trash_retention_days,
    (doc, value) => ({ ...doc, trash_retention_days: value }),
  ),
  'index.concurrency': inRange(
    SCAN_CONCURRENCY,
    (doc) => doc.index.concurrency,
    (doc, value) => ({ ...doc, index: { ...doc.index, concurrency: value } }),
  ),
  'index.hash_concurrency': inRange(
    HASH_CONCURRENCY,
    (doc) => doc.index.hash_concurrency,
    (doc, value) => ({ ...doc, index: { ...doc.index, hash_concurrency: value } }),
  ),
  'index.thumbnail_concurrency': inRange(
    THUMBNAIL_CONCURRENCY,
    (doc) => doc.index.thumbnail_concurrency,
    (doc, value) => ({ ...doc, index: { ...doc.index, thumbnail_concurrency: value } }),
  ),
  'index.rescan_interval_minutes': {
    read: (doc) => doc.index.rescan_interval_minutes,
    write: (doc, value) => ({ ...doc, index: { ...doc.index, rescan_interval_minutes: value } }),
    min: RESCAN_NEVER,
    max: RESCAN_INTERVAL_MINUTES.max,
    valid: isValidRescanInterval,
    message: `Enter 0 to never rescan, or a whole number of minutes from ${RESCAN_INTERVAL_MINUTES.min} to ${RESCAN_INTERVAL_MINUTES.max}.`,
  },
};

const NUMBER_FIELDS = Object.keys(NUMBER_RULES) as NumberField[];

/** Why the document's value of `field` is not acceptable, else `null`. */
function numberProblem(doc: SettingsDto, field: NumberField): string | null {
  const rule = NUMBER_RULES[field];
  const value = rule.read(doc);
  return value === null || rule.valid(value) ? null : rule.message;
}

const NOTE_LISTS =
  'Applies to the next scan. Use Re-index in the menu to apply it to files that are already indexed.';
const NOTE_WORKERS =
  'Read when a scan or batch starts, so a change applies to the next one. Low values are right for a NAS CPU.';

const CARD = 'fieldset gap-4 rounded-box border border-base-300 bg-base-200 p-4 text-sm';

const NO_INDEXES: readonly number[] = [];
const SHOWN_PROBLEMS = 3;

interface FieldProblem {
  message: string;
  /** Positions of the list entries the server refused. */
  indexes: readonly number[];
}

/** `sidecar_extensions[2]`: the field and the position in the list that was sent. */
const ENTRY_PATH = /^(.+)\[(\d+)\]$/;

const isField = (path: string): path is FieldId =>
  (FIELD_ORDER as readonly string[]).includes(path);
const isListField = (field: FieldId): field is ListField =>
  (LIST_FIELDS as readonly string[]).includes(field);

const capitalize = (text: string): string => text.charAt(0).toUpperCase() + text.slice(1);

const domId = (key: string): string => `settings-general-${key.replaceAll('.', '-')}`;

/** Compares two JSON documents by value. */
function same(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  return (
    keys.length === Object.keys(right).length &&
    keys.every((key) => key in right && same(left[key], right[key]))
  );
}

/** An empty box is `NaN` (never valid) rather than 0, which is a real value for the rescan. */
function parseNumber(text: string): number {
  return text.trim() === '' ? Number.NaN : Number(text);
}

function describeInterval(minutes: number): string {
  if (minutes === RESCAN_NEVER) return 'Never rescans on its own.';
  const parts: string[] = [];
  const add = (count: number, unit: string): void => {
    if (count > 0) parts.push(`${count} ${unit}${count === 1 ? '' : 's'}`);
  };
  add(Math.floor(minutes / 1440), 'day');
  add(Math.floor((minutes % 1440) / 60), 'hour');
  add(minutes % 60, 'minute');
  return `Every ${parts.join(' ')}.`;
}

function summarize(lines: readonly string[]): string {
  const shown = lines.slice(0, SHOWN_PROBLEMS).join('; ');
  const more = lines.length - SHOWN_PROBLEMS;
  return more > 0 ? `${shown}; and ${more} more` : shown;
}

/**
 * Maps the server's `fields` onto the form. A path such as `sidecar_extensions[2]` points at an
 * entry of the list that was sent, so it marks that chip and quotes the entry; a bare field name
 * is a problem of the whole field. Whatever has no field to show it comes back as `unmapped`.
 */
function sortProblems(
  fields: readonly FieldError[],
  sent: SettingsDto,
): { problems: Map<FieldId, FieldProblem>; unmapped: string[] } {
  const lines = new Map<FieldId, string[]>();
  const indexes = new Map<FieldId, number[]>();
  const unmapped: string[] = [];

  for (const { field, message } of fields) {
    const match = ENTRY_PATH.exec(field);
    const path = match?.[1] ?? field;
    const position = match?.[2] ? Number(match[2]) : null;
    if (!isField(path) || (position !== null && !isListField(path))) {
      unmapped.push(`${field}: ${message}`);
      continue;
    }
    let line = capitalize(message);
    if (position !== null && isListField(path)) {
      const entry: string | undefined = sent[path][position];
      if (entry !== undefined) line = `"${entry}" ${message}`;
      indexes.set(path, [...(indexes.get(path) ?? []), position]);
    }
    lines.set(path, [...(lines.get(path) ?? []), line]);
  }

  const problems = new Map<FieldId, FieldProblem>();
  for (const [field, found] of lines) {
    problems.set(field, { message: summarize(found), indexes: indexes.get(field) ?? NO_INDEXES });
  }
  return { problems, unmapped };
}

function note(text: string) {
  return html`
    <p class="flex items-start gap-2 text-xs text-base-content/70">
      <apm-icon name="info" size="xs" class="mt-0.5"></apm-icon>
      <span>${text}</span>
    </p>
  `;
}

interface NumberFieldSpec {
  field: NumberField;
  label: string;
  unit?: string;
  hint: string;
  /** Shown while the document has no value (the retention while the deletion is off). */
  fallback?: number;
  disabled?: boolean;
}

interface ToggleSpec {
  /** Makes the DOM id of the switch. */
  key: string;
  label: string;
  help: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}

/**
 * The "General" tab of the settings page: the sidecar and junk lists, the move and trash
 * behaviour and the indexing workers, all in the one server-side document of the `settings` store.
 *
 * The form edits a copy (`draft`) of the loaded document and always sends the whole copy, so
 * fields it does not know survive a save. A sticky bar with Save and Discard appears while the copy
 * differs from what the server has. Saving replaces the form with the document the server answers
 * with (extensions lowercased and de-duplicated, ...). The server stays the judge: a 422 puts its
 * reasons on the fields it names, and the checks here only mirror its limits (`limits.ts`).
 */
export class ApmSettingsGeneral extends ApmElement {
  static override properties = {
    draft: { state: true },
    baseline: { state: true },
    loadError: { state: true },
    saving: { state: true },
    fieldProblems: { state: true },
    alertLines: { state: true },
  };

  /** What the form shows and edits; `null` until the settings have loaded. */
  declare draft: SettingsDto | null;
  /** What the server has, the form's reference for "unsaved changes". */
  declare baseline: SettingsDto | null;
  declare loadError: string | null;
  declare saving: boolean;
  declare fieldProblems: ReadonlyMap<FieldId, FieldProblem>;
  /** Problems of a save that no field can show; displayed in an alert above the form. */
  declare alertLines: readonly string[];

  /** The days the automatic deletion is turned back on with. */
  private lastRetention = DEFAULT_TRASH_RETENTION_DAYS;
  private loads = 0;
  private readonly unsavedGuard = new UnsavedGuard(this, () => this.dirty);
  private pendingFocus: FieldId | 'alert' | null = null;

  constructor() {
    super();
    this.draft = null;
    this.baseline = null;
    this.loadError = null;
    this.saving = false;
    this.fieldProblems = new Map();
    this.alertLines = [];
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
    if (this.baseline === null) void this.load();
  }

  protected override updated(): void {
    this.unsavedGuard.sync();
    if (this.saving || this.pendingFocus === null) return;
    const target = this.pendingFocus;
    this.pendingFocus = null;
    if (target === 'alert') {
      this.querySelector('[data-alert]')?.scrollIntoView?.({ block: 'nearest' });
      return;
    }
    const field = this.querySelector<HTMLElement>(`[data-field="${target}"]`);
    // A chip list enables its own text box in its own update, which follows this one.
    const ready = field instanceof ApmChipInput ? field.updateComplete : Promise.resolve();
    void ready.then(() => field?.focus());
  }

  private get dirty(): boolean {
    return this.draft !== null && this.baseline !== null && !same(this.draft, this.baseline);
  }

  private get invalid(): boolean {
    const doc = this.draft;
    return doc !== null && NUMBER_FIELDS.some((field) => numberProblem(doc, field) !== null);
  }

  private async load(): Promise<void> {
    const token = ++this.loads;
    this.loadError = null;
    try {
      const doc = await loadSettings();
      if (token === this.loads) this.adopt(doc);
    } catch (error) {
      if (token === this.loads) this.loadError = errorMessage(error, 'Could not load the settings');
    }
  }

  /** Makes `doc` both the server's state and the form's, clearing every message. */
  private adopt(doc: SettingsDto): void {
    this.baseline = doc;
    this.draft = doc;
    this.loadError = null;
    this.fieldProblems = new Map();
    this.alertLines = [];
    if (doc.trash_retention_days !== null) this.lastRetention = doc.trash_retention_days;
  }

  private discard(): void {
    if (this.baseline !== null) this.adopt(this.baseline);
  }

  private async save(): Promise<void> {
    const sent = this.draft;
    if (sent === null || this.saving || this.invalid) return;
    this.saving = true;
    this.fieldProblems = new Map();
    this.alertLines = [];
    try {
      this.adopt(await saveSettings(sent));
      toast.success('Settings saved');
    } catch (error) {
      this.fail(error, sent);
    } finally {
      this.saving = false;
    }
  }

  /** Shows why `sent` was refused. Everything but a rejected value leaves the form as it is. */
  private fail(error: unknown, sent: SettingsDto): void {
    if (isApiError(error) && error.code === 'validation_failed' && error.fields.length > 0) {
      const { problems, unmapped } = sortProblems(error.fields, sent);
      this.fieldProblems = problems;
      this.alertLines = unmapped;
      this.pendingFocus =
        unmapped.length > 0 ? 'alert' : (FIELD_ORDER.find((field) => problems.has(field)) ?? null);
    } else if (isApiError(error) && ['validation_failed', 'invalid_json'].includes(error.code)) {
      this.alertLines = [error.message];
      this.pendingFocus = 'alert';
    } else {
      toast.error(errorMessage(error, 'Could not save the settings'));
    }
  }

  private change(edit: (doc: SettingsDto) => SettingsDto, field?: FieldId): void {
    if (this.draft === null) return;
    this.draft = edit(this.draft);
    if (field !== undefined && this.fieldProblems.has(field)) {
      const rest = new Map(this.fieldProblems);
      rest.delete(field);
      this.fieldProblems = rest;
    }
  }

  private setList(field: ListField, values: string[]): void {
    const patch: Partial<Record<ListField, string[]>> = {};
    patch[field] = values;
    this.change((doc) => ({ ...doc, ...patch }), field);
  }

  private setNumber(field: NumberField, value: number): void {
    if (field === 'trash_retention_days' && isInRange(value, TRASH_RETENTION_DAYS)) {
      this.lastRetention = value;
    }
    this.change((doc) => NUMBER_RULES[field].write(doc, value), field);
  }

  private setAutoDelete(on: boolean): void {
    this.change(
      (doc) => ({ ...doc, trash_retention_days: on ? this.lastRetention : null }),
      'trash_retention_days',
    );
  }

  protected override render() {
    const doc = this.draft;
    if (doc === null) {
      return this.loadError === null ? this.renderSkeleton() : this.renderLoadError(this.loadError);
    }
    return html`
      <form
        class="flex flex-col gap-4"
        aria-label="General settings"
        novalidate
        @submit=${(event: Event) => event.preventDefault()}
      >
        ${this.renderAlert()} ${this.renderFiles(doc)} ${this.renderMoving(doc)}
        ${this.renderIndexing(doc)} ${this.dirty ? this.renderActionBar() : nothing}
      </form>
    `;
  }

  private renderSkeleton() {
    return html`
      <div class="flex flex-col gap-4" role="status" aria-label="Loading the settings">
        <div class="skeleton h-72 w-full"></div>
        <div class="skeleton h-72 w-full"></div>
        <div class="skeleton h-72 w-full"></div>
      </div>
    `;
  }

  private renderLoadError(message: string) {
    return html`
      <div role="alert" class="alert alert-error alert-soft">
        <apm-icon name="circle-alert"></apm-icon>
        <div>
          <h3 class="font-semibold">The settings could not be loaded</h3>
          <p class="text-sm">${message}</p>
        </div>
        <button type="button" class="btn btn-sm" @click=${() => this.load()}>Retry</button>
      </div>
    `;
  }

  private renderAlert() {
    if (this.alertLines.length === 0) return nothing;
    return html`
      <div role="alert" class="alert alert-error alert-soft" data-alert>
        <apm-icon name="circle-alert"></apm-icon>
        <div>
          <h3 class="font-semibold">The settings were not saved</h3>
          <ul class="list-disc pl-4 text-sm">
            ${this.alertLines.map((line) => html`<li>${line}</li>`)}
          </ul>
        </div>
      </div>
    `;
  }

  private renderFiles(doc: SettingsDto) {
    return html`
      <fieldset class=${CARD} ?disabled=${this.saving}>
        <legend class="fieldset-legend text-base">Files and sidecars</legend>
        ${LIST_FIELDS.map((field) => this.listField(doc, field))} ${note(NOTE_LISTS)}
        ${this.numberField(doc, {
          field: 'name_max_len',
          label: 'Longest generated name',
          unit: 'bytes',
          hint: `Names the app generates, such as name_1.jpg, stay within this length. Encrypted shares only allow ${ENCRYPTED_SHARE_NAME_LEN}.`,
        })}
      </fieldset>
    `;
  }

  private renderMoving(doc: SettingsDto) {
    const policyId = domId('conflict-policy');
    const policy = CONFLICT_POLICIES.find((entry) => entry.value === doc.default_conflict_policy);
    const autoDelete = doc.trash_retention_days !== null;
    return html`
      <fieldset class=${CARD} ?disabled=${this.saving}>
        <legend class="fieldset-legend text-base">Moving and deleting</legend>
        <div class="flex flex-col gap-1">
          <span class="text-sm font-medium">When a file with the same name already exists</span>
          <apm-select
            id=${policyId}
            size="md"
            data-field="default_conflict_policy"
            label="When a file with the same name already exists"
            .options=${CONFLICT_POLICIES.map((entry) => ({ ...entry, title: entry.help }))}
            .value=${doc.default_conflict_policy}
            .disabled=${this.saving}
            aria-describedby="${policyId}-help"
            @select-change=${(event: CustomEvent<{ value: string }>) => {
              const value = event.detail.value;
              if (isConflictPolicy(value))
                this.change((current) => ({ ...current, default_conflict_policy: value }));
            }}
          ></apm-select>
          <p id="${policyId}-help" class="text-xs text-base-content/70">${policy?.help}</p>
        </div>
        ${this.toggle({
          key: 'keep-cross-volume-source',
          label: 'Keep the original in the trash after moving a file to another volume',
          help: 'Uses extra space until the trash is emptied.',
          checked: doc.keep_cross_volume_source_in_trash,
          onChange: (checked) =>
            this.change((current) => ({
              ...current,
              keep_cross_volume_source_in_trash: checked,
            })),
        })}
        ${this.toggle({
          key: 'trash-auto-delete',
          label: 'Delete trashed files automatically',
          help: 'When off, trashed files stay until you empty the trash.',
          checked: autoDelete,
          onChange: (checked) => this.setAutoDelete(checked),
        })}
        ${this.numberField(doc, {
          field: 'trash_retention_days',
          label: 'Delete after',
          unit: 'days',
          hint: `${TRASH_RETENTION_DAYS.min} to ${TRASH_RETENTION_DAYS.max} days after a file was trashed.`,
          fallback: this.lastRetention,
          disabled: !autoDelete,
        })}
      </fieldset>
    `;
  }

  private renderIndexing(doc: SettingsDto) {
    const interval = doc.index.rescan_interval_minutes;
    const intervalHint = NUMBER_RULES['index.rescan_interval_minutes'].valid(interval)
      ? `${describeInterval(interval)} Use 0 to never rescan.`
      : '';
    return html`
      <fieldset class=${CARD} ?disabled=${this.saving}>
        <legend class="fieldset-legend text-base">Indexing</legend>
        <div class="grid gap-4 sm:grid-cols-3">
          ${this.numberField(doc, {
            field: 'index.concurrency',
            label: 'Scan workers',
            hint: 'Volumes scanned at once.',
          })}
          ${this.numberField(doc, {
            field: 'index.hash_concurrency',
            label: 'Hash workers',
            hint: 'Files hashed at once.',
          })}
          ${this.numberField(doc, {
            field: 'index.thumbnail_concurrency',
            label: 'Thumbnail workers',
            hint: 'Thumbnails made at once.',
          })}
        </div>
        ${note(NOTE_WORKERS)}
        ${this.toggle({
          key: 'watch',
          label: 'Watch for changes',
          help: 'Watch the volumes and re-index files as they change.',
          checked: doc.index.watch_enabled,
          onChange: (checked) =>
            this.change((current) => ({
              ...current,
              index: { ...current.index, watch_enabled: checked },
            })),
        })}
        ${this.numberField(doc, {
          field: 'index.rescan_interval_minutes',
          label: 'Full rescan interval',
          unit: 'minutes',
          hint: intervalHint,
        })}
      </fieldset>
    `;
  }

  private listField(doc: SettingsDto, field: ListField) {
    const spec = LISTS[field];
    const problem = this.fieldProblems.get(field);
    return html`
      <apm-chip-input
        data-field=${field}
        label=${spec.label}
        placeholder=${spec.placeholder}
        hint=${spec.hint}
        ?allow-spaces=${spec.allowSpaces}
        .values=${doc[field]}
        .max=${spec.max}
        .normalizeEntry=${spec.normalize}
        .validateEntry=${spec.validate}
        .error=${problem?.message ?? ''}
        .errorIndexes=${problem?.indexes ?? NO_INDEXES}
        .disabled=${this.saving}
        @values-change=${(event: ChipValuesChange) => this.setList(field, event.detail.values)}
      ></apm-chip-input>
    `;
  }

  private numberField(doc: SettingsDto, spec: NumberFieldSpec) {
    const rule = NUMBER_RULES[spec.field];
    const value = rule.read(doc) ?? spec.fallback ?? Number.NaN;
    const message =
      this.fieldProblems.get(spec.field)?.message ?? numberProblem(doc, spec.field) ?? '';
    const invalid = message !== '';
    const inputId = domId(spec.field);
    const messageId = `${inputId}-message`;

    return html`
      <div class="flex flex-col gap-1">
        <label for=${inputId} class="text-sm font-medium">${spec.label}</label>
        <div class="input w-40 ${invalid ? 'validator' : ''}">
          <input
            id=${inputId}
            data-field=${spec.field}
            type="number"
            inputmode="numeric"
            min=${rule.min}
            max=${rule.max}
            step="1"
            .value=${live(Number.isNaN(value) ? '' : String(value))}
            ?disabled=${spec.disabled === true || this.saving}
            aria-invalid=${invalid ? 'true' : nothing}
            aria-describedby=${messageId}
            @input=${(event: Event) =>
              this.setNumber(spec.field, parseNumber((event.target as HTMLInputElement).value))}
          />
          ${spec.unit ? html`<span class="label">${spec.unit}</span>` : nothing}
        </div>
        <p
          id=${messageId}
          class=${invalid ? 'validator-hint mt-0' : 'text-xs text-base-content/70'}
          role=${invalid ? 'alert' : nothing}
        >
          ${invalid ? message : spec.hint}
        </p>
      </div>
    `;
  }

  private toggle(spec: ToggleSpec) {
    const inputId = domId(spec.key);
    return html`
      <div class="flex items-start justify-between gap-4">
        <div class="flex min-w-0 flex-col gap-1">
          <label for=${inputId} class="cursor-pointer text-sm font-medium">${spec.label}</label>
          <p id="${inputId}-help" class="text-xs text-base-content/70">${spec.help}</p>
        </div>
        <input
          id=${inputId}
          type="checkbox"
          role="switch"
          class="toggle toggle-primary mt-0.5 shrink-0"
          .checked=${live(spec.checked)}
          ?disabled=${this.saving}
          aria-describedby="${inputId}-help"
          @change=${(event: Event) => spec.onChange((event.target as HTMLInputElement).checked)}
        />
      </div>
    `;
  }

  private renderActionBar() {
    return html`<apm-save-bar .invalid=${this.invalid} .saving=${this.saving}
      @discard=${() => this.discard()} @save=${() => this.save()}></apm-save-bar>`;
  }
}

customElements.define('apm-settings-general', ApmSettingsGeneral);

declare global {
  interface HTMLElementTagNameMap {
    'apm-settings-general': ApmSettingsGeneral;
  }
}
