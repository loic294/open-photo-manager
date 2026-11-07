import { html, nothing } from 'lit';
import { unsafeSVG } from 'lit/directives/unsafe-svg.js';
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowRightLeft,
  ArrowUp,
  ArrowUpDown,
  Ban,
  Calendar,
  Camera,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronsUpDown,
  ChevronUp,
  CircleAlert,
  CircleCheck,
  CircleQuestionMark,
  CircleX,
  Clock,
  Copy,
  Database,
  Download,
  Ellipsis,
  EllipsisVertical,
  ExternalLink,
  Eye,
  EyeOff,
  File,
  Film,
  Flag,
  Folder,
  FolderInput,
  FolderOpen,
  FolderPlus,
  FolderTree,
  FolderX,
  Funnel,
  HardDrive,
  House,
  type IconNode,
  Image,
  ImageOff,
  Images,
  Info,
  Key,
  LayoutGrid,
  Link,
  List,
  LoaderCircle,
  Lock,
  Maximize,
  Menu,
  Minimize,
  Minus,
  Monitor,
  Moon,
  Pause,
  Pencil,
  Play,
  Plug,
  Plus,
  RefreshCw,
  RotateCcw,
  RotateCw,
  Save,
  Search,
  Settings,
  Square,
  SquareCheck,
  Star,
  Sun,
  Tag,
  Trash2,
  TriangleAlert,
  Undo2,
  Upload,
  Wifi,
  WifiOff,
  X,
  ZoomIn,
  ZoomOut,
} from 'lucide';
import { ApmElement } from '@/base/apm-element';

/**
 * The icon set: kebab-case Lucide names (https://lucide.dev/icons). Import Lucide icons by name
 * only (never `icons` or `createIcons`), so the bundler keeps just the ones listed here. Add a
 * line (alphabetical) when a component needs another icon.
 */
const ICONS = {
  'arrow-down': ArrowDown,
  'arrow-left': ArrowLeft,
  'arrow-right': ArrowRight,
  'arrow-right-left': ArrowRightLeft,
  'arrow-up': ArrowUp,
  'arrow-up-down': ArrowUpDown,
  ban: Ban,
  calendar: Calendar,
  camera: Camera,
  check: Check,
  'chevron-down': ChevronDown,
  'chevron-left': ChevronLeft,
  'chevron-right': ChevronRight,
  'chevron-up': ChevronUp,
  'chevrons-up-down': ChevronsUpDown,
  'circle-alert': CircleAlert,
  'circle-check': CircleCheck,
  'circle-question-mark': CircleQuestionMark,
  'circle-x': CircleX,
  clock: Clock,
  copy: Copy,
  database: Database,
  download: Download,
  ellipsis: Ellipsis,
  'ellipsis-vertical': EllipsisVertical,
  'external-link': ExternalLink,
  eye: Eye,
  'eye-off': EyeOff,
  file: File,
  film: Film,
  flag: Flag,
  folder: Folder,
  'folder-input': FolderInput,
  'folder-open': FolderOpen,
  'folder-plus': FolderPlus,
  'folder-tree': FolderTree,
  'folder-x': FolderX,
  funnel: Funnel,
  'hard-drive': HardDrive,
  house: House,
  image: Image,
  'image-off': ImageOff,
  images: Images,
  info: Info,
  key: Key,
  'layout-grid': LayoutGrid,
  link: Link,
  list: List,
  'loader-circle': LoaderCircle,
  lock: Lock,
  maximize: Maximize,
  menu: Menu,
  minimize: Minimize,
  minus: Minus,
  monitor: Monitor,
  moon: Moon,
  pause: Pause,
  pencil: Pencil,
  play: Play,
  plug: Plug,
  plus: Plus,
  'refresh-cw': RefreshCw,
  'rotate-ccw': RotateCcw,
  'rotate-cw': RotateCw,
  save: Save,
  search: Search,
  settings: Settings,
  square: Square,
  'square-check': SquareCheck,
  star: Star,
  sun: Sun,
  tag: Tag,
  'trash-2': Trash2,
  'triangle-alert': TriangleAlert,
  'undo-2': Undo2,
  upload: Upload,
  wifi: Wifi,
  'wifi-off': WifiOff,
  x: X,
  'zoom-in': ZoomIn,
  'zoom-out': ZoomOut,
} satisfies Record<string, IconNode>;

export type IconName = keyof typeof ICONS;

export const ICON_NAMES = Object.keys(ICONS) as IconName[];

export function isIconName(value: unknown): value is IconName {
  return typeof value === 'string' && Object.hasOwn(ICONS, value);
}

/** Tailwind needs these as complete strings. */
const SIZE_CLASS = {
  xs: 'size-3',
  sm: 'size-4',
  md: 'size-5',
  lg: 'size-6',
  xl: 'size-8',
} as const;

export type IconSize = keyof typeof SIZE_CLASS;

const markupCache = new Map<IconName, string>();

function markupOf(name: IconName): string {
  let markup = markupCache.get(name);
  if (markup === undefined) {
    markup = ICONS[name]
      .map(([tag, attrs]) => {
        const attributes = Object.entries(attrs).map(([key, value]) => `${key}="${value}"`);
        return `<${tag} ${attributes.join(' ')}/>`;
      })
      .join('');
    markupCache.set(name, markup);
  }
  return markup;
}

/**
 * An inline SVG icon that takes the text colour of its parent.
 *
 *   <apm-icon name="trash-2"></apm-icon>
 *   <apm-icon name="check" size="sm" label="Saved"></apm-icon>
 *
 * Decorative by default (`aria-hidden`); set `label` when the icon is the only content, then it
 * becomes `role="img"` with that accessible name.
 */
export class ApmIcon extends ApmElement {
  static override properties = {
    name: { type: String },
    size: { type: String },
    label: { type: String },
  };

  declare name: IconName;
  declare size: IconSize;
  declare label: string | undefined;

  constructor() {
    super();
    this.size = 'md';
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('inline-flex', 'shrink-0', 'align-middle');
  }

  protected override render() {
    if (!isIconName(this.name)) return nothing;
    return html`<svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="block ${SIZE_CLASS[this.size] ?? SIZE_CLASS.md}"
      role=${this.label ? 'img' : nothing}
      aria-label=${this.label || nothing}
      aria-hidden=${this.label ? nothing : 'true'}
    >${unsafeSVG(markupOf(this.name))}</svg>`;
  }
}

customElements.define('apm-icon', ApmIcon);

declare global {
  interface HTMLElementTagNameMap {
    'apm-icon': ApmIcon;
  }
}
