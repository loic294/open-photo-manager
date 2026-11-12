import type { IconName } from '@/components/opm-icon';

/**
 * One tab of the settings page.
 *
 * To add a section, append ONE entry to `settingsSections` below and change nothing else: the page
 * (`opm-settings-page`) builds its tabs from the list, keeps the active `id` in `?section=<id>`
 * and calls `load()` the first time the tab is opened. The section is a custom element that is
 * independent of the page: it loads and saves its own data (the general section uses the
 * `settings` store, another one may call its own endpoint) and reports its own errors. The page
 * renders `<tag>` without properties or children, so the section needs nothing from it.
 *
 * An opened section stays in the page, hidden while another tab is open, so that edits which are
 * not saved yet survive a switch of tabs. It is therefore not disconnected when its tab is left:
 * a section must not poll or keep a stream open while hidden (`this.checkVisibility()` is false
 * then).
 */
export interface SettingsSection {
  /** Unique; lowercase letters, digits and hyphens. It is the value of `?section=`. */
  id: string;
  /** The text of the tab. */
  label: string;
  /** One of the icons registered in `components/opm-icon.ts`, shown before the label. */
  icon: IconName;
  /** The custom element that is the section's panel. `load()` must define it. */
  tag: string;
  /** Imports the module that defines `tag`: `() => import('@/tools/settings/opm-settings-x')`. */
  load: () => Promise<unknown>;
}

/** The tabs in display order; the first one is shown when the URL names no (known) section. */
export const settingsSections: readonly SettingsSection[] = [
  {
    id: 'general',
    label: 'General',
    icon: 'settings',
    tag: 'opm-settings-general',
    load: () => import('@/tools/settings/opm-settings-general'),
  },
  {
    id: 'immich',
    label: 'Immich',
    icon: 'plug',
    tag: 'opm-settings-immich',
    load: () => import('@/tools/settings/opm-settings-immich'),
  },
];
