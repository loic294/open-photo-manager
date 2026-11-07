import { afterEach } from 'vitest';

const mounted = new Set<HTMLElement>();

afterEach(() => {
  for (const element of mounted) {
    element.remove();
  }
  mounted.clear();
});

/**
 * Appends an element to `document.body`, waits for its first render and removes it after the
 * current test. Props are assigned before the element is connected.
 *
 * Only the mounted element's own `updateComplete` is awaited: await `child.updateComplete`
 * yourself for nested components.
 */
export async function mount<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: Partial<HTMLElementTagNameMap[K]>,
): Promise<HTMLElementTagNameMap[K]>;
export async function mount<T extends HTMLElement>(element: T, props?: Partial<T>): Promise<T>;
export async function mount(tag: string, props?: Record<string, unknown>): Promise<HTMLElement>;
export async function mount(
  target: string | HTMLElement,
  props: object = {},
): Promise<HTMLElement> {
  if (typeof target === 'string' && !customElements.get(target)) {
    throw new Error(`mount(): <${target}> is not registered. Import its module first.`);
  }
  const element = typeof target === 'string' ? document.createElement(target) : target;
  Object.assign(element, props);
  document.body.append(element);
  mounted.add(element);
  await (element as Partial<{ updateComplete: Promise<unknown> }>).updateComplete;
  return element;
}
