import { signal } from '@preact/signals-core';
import { html } from 'lit';
import { beforeEach, describe, expect, it } from 'vitest';
import { ApmElement } from '@/base/apm-element';
import { mount } from '@/test/helpers';

const count = signal(0);

class ApmTestReactive extends ApmElement {
  static override properties = {
    label: { type: String },
    clicks: { type: Number },
    note: { type: String },
  };

  label = 'initial';
  clicks = 0;
  declare note: string | undefined;

  protected override render() {
    return html`
      <button type="button" class="btn btn-primary">${this.label}</button>
      <span class="clicks">${this.clicks}</span>
      <span class="note">${this.note ?? 'none'}</span>
    `;
  }
}

class ApmTestSignal extends ApmElement {
  protected override render() {
    return html`<span class="count">${count.value}</span>`;
  }
}

customElements.define('apm-test-reactive', ApmTestReactive);
customElements.define('apm-test-signal', ApmTestSignal);

declare global {
  interface HTMLElementTagNameMap {
    'apm-test-reactive': ApmTestReactive;
    'apm-test-signal': ApmTestSignal;
  }
}

describe('ApmElement', () => {
  beforeEach(() => {
    count.value = 0;
  });

  it('renders the initial property values', async () => {
    const element = await mount('apm-test-reactive');
    expect(element.querySelector('button')?.textContent).toBe('initial');
    expect(element.querySelector('.clicks')?.textContent).toBe('0');
    expect(element.querySelector('.note')?.textContent).toBe('none');
  });

  it('applies props passed to mount() before the first render', async () => {
    const element = await mount('apm-test-reactive', { label: 'from mount', clicks: 3 });
    expect(element.querySelector('button')?.textContent).toBe('from mount');
    expect(element.querySelector('.clicks')?.textContent).toBe('3');
  });

  it('re-renders when a reactive property changes', async () => {
    const element = await mount('apm-test-reactive');
    element.label = 'changed';
    element.clicks = 2;
    element.note = 'set later';
    await element.updateComplete;
    expect(element.querySelector('button')?.textContent).toBe('changed');
    expect(element.querySelector('.clicks')?.textContent).toBe('2');
    expect(element.querySelector('.note')?.textContent).toBe('set later');
  });

  it('keeps field initialisers from shadowing the reactive accessors', async () => {
    // Guards `useDefineForClassFields: false`: native class fields would create own properties
    // that bypass Lit's accessors, so changes would no longer re-render.
    const element = await mount('apm-test-reactive');
    expect(Object.hasOwn(element, 'label')).toBe(false);
    expect(Object.hasOwn(element, 'clicks')).toBe(false);
  });

  it('re-renders when an attribute changes', async () => {
    const element = await mount('apm-test-reactive');
    element.setAttribute('label', 'from attribute');
    await element.updateComplete;
    expect(element.label).toBe('from attribute');
    expect(element.querySelector('button')?.textContent).toBe('from attribute');
  });

  it('re-renders when a signal read inside render() changes', async () => {
    const element = await mount('apm-test-signal');
    expect(element.querySelector('.count')?.textContent).toBe('0');
    count.value = 7;
    await element.updateComplete;
    expect(element.querySelector('.count')?.textContent).toBe('7');
  });

  it('renders into the light DOM', async () => {
    const element = await mount('apm-test-reactive');
    expect(element.shadowRoot).toBeNull();
    expect(element.querySelector('button')).not.toBeNull();
    expect(document.body.querySelector('apm-test-reactive button')).not.toBeNull();
  });

  it('renders DaisyUI classes as plain class names', async () => {
    const element = await mount('apm-test-reactive');
    const button = element.querySelector('button');
    expect(button?.className).toBe('btn btn-primary');
    expect(button?.classList.contains('btn-primary')).toBe(true);
  });

  it('does not project or replace children written in markup (no slots)', async () => {
    const element = document.createElement('apm-test-reactive');
    element.append(document.createElement('em'));
    await mount(element);
    expect(element.querySelector('slot')).toBeNull();
    expect(element.firstElementChild?.localName).toBe('em');
    expect(element.querySelector('em')?.nextElementSibling?.localName).toBe('button');
    element.label = 'changed';
    await element.updateComplete;
    expect(element.querySelectorAll('em')).toHaveLength(1);
    expect(element.querySelector('button')?.textContent).toBe('changed');
  });

  it('removes mounted elements after each test', () => {
    expect(document.body.querySelector('apm-test-reactive')).toBeNull();
    expect(document.body.querySelector('apm-test-signal')).toBeNull();
  });
});
