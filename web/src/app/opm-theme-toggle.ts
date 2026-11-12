import { html } from 'lit';
import { live } from 'lit/directives/live.js';
import { nextTheme, THEME_LABELS } from '@/app/theme';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-icon';
import { theme } from '@/stores/prefs';

/**
 * Cycles the `theme` preference system -> light -> dark -> system. It is one DaisyUI `swap`
 * whose checkbox has three states: indeterminate = system (monitor), unchecked = light (sun),
 * checked = dark (moon). The choice persists through the preference; `startTheme()` (started by
 * the shell) applies it to the page.
 *
 *   <opm-theme-toggle></opm-theme-toggle>
 */
export class OpmThemeToggle extends OpmElement {
  private onChange() {
    theme.value = nextTheme(theme.peek());
  }

  protected override render() {
    const pref = theme.value;
    const state = `Theme: ${THEME_LABELS[pref]}`;
    return html`
      <div class="tooltip tooltip-left" data-tip=${state}>
        <label
          class="btn btn-ghost btn-sm btn-square swap swap-rotate has-focus-visible:outline-2 has-focus-visible:outline-offset-2 has-focus-visible:outline-primary"
        >
          <input
            type="checkbox"
            aria-label="${state}. Switch to ${THEME_LABELS[nextTheme(pref)]}."
            .checked=${live(pref === 'dark')}
            .indeterminate=${live(pref === 'system')}
            @change=${this.onChange}
          />
          <opm-icon class="swap-on" name="moon"></opm-icon>
          <opm-icon class="swap-off" name="sun"></opm-icon>
          <opm-icon class="swap-indeterminate" name="monitor"></opm-icon>
        </label>
      </div>
    `;
  }
}

customElements.define('opm-theme-toggle', OpmThemeToggle);

declare global {
  interface HTMLElementTagNameMap {
    'opm-theme-toggle': OpmThemeToggle;
  }
}
