import { SignalWatcher } from '@lit-labs/preact-signals';
import { LitElement } from 'lit';

/**
 * Base class for every OPM component.
 *
 * Renders into the LIGHT DOM (`this`, no shadow root), so DaisyUI / Tailwind classes apply
 * directly and components need no CSS of their own. Signals read inside `render()` are tracked:
 * when one changes, the element re-renders.
 *
 * Light-DOM caveats:
 * - There are no slots: `<slot>` does nothing without a shadow root.
 * - Children written in markup are not projected into the template and Lit does not manage them.
 *   They stay in the DOM before the rendered output, so never author children on a component.
 *   Compose through properties and templates: pass data as properties and render child
 *   components from `render()`.
 * - `static styles`, `:host` and `::slotted` have no effect (nothing is encapsulated).
 */
export abstract class OpmElement extends SignalWatcher(LitElement) {
  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }
}
