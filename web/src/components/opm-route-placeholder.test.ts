import { describe, expect, it } from 'vitest';
import '@/components/opm-route-placeholder';
import { mount } from '@/test/helpers';

describe('opm-route-placeholder', () => {
  it('says "Coming soon" by default', async () => {
    const view = await mount('opm-route-placeholder', { heading: 'Duplicates' });

    expect(view.querySelector('h2')?.textContent).toBe('Duplicates');
    expect(view.querySelector('p')?.textContent).toBe('Coming soon');
  });

  it('leaves the page <h1> to the shell', async () => {
    const view = await mount('opm-route-placeholder', { heading: 'Duplicates' });

    expect(view.querySelector('h1')).toBeNull();
  });

  it('shows a custom message', async () => {
    const view = await mount('opm-route-placeholder', {
      heading: 'Page not found',
      message: 'There is nothing at /nope.',
    });

    expect(view.querySelector('p')?.textContent).toBe('There is nothing at /nope.');
  });

  it('shows the icon when given one, and none otherwise', async () => {
    const withIcon = await mount('opm-route-placeholder', { heading: 'x', icon: 'copy' });
    const without = await mount('opm-route-placeholder', { heading: 'y' });

    expect(withIcon.querySelector('opm-icon')?.getAttribute('name')).toBe('copy');
    expect(without.querySelector('opm-icon')).toBeNull();
  });

  it('omits the heading when there is none', async () => {
    const view = await mount('opm-route-placeholder');

    expect(view.querySelector('h2')).toBeNull();
  });

  it('offers a link only when it has both an address and a label', async () => {
    const full = await mount('opm-route-placeholder', {
      actionHref: '/browse',
      actionLabel: 'Go to Browse',
    });
    const noLabel = await mount('opm-route-placeholder', { actionHref: '/browse' });
    const noHref = await mount('opm-route-placeholder', { actionLabel: 'Go' });

    const link = full.querySelector('a.btn');
    expect(link?.getAttribute('href')).toBe('/browse');
    expect(link?.textContent).toBe('Go to Browse');
    expect(noLabel.querySelector('a')).toBeNull();
    expect(noHref.querySelector('a')).toBeNull();
  });

  it('reads the action from attributes too', async () => {
    const view = document.createElement('opm-route-placeholder');
    view.setAttribute('action-href', '/trash');
    view.setAttribute('action-label', 'Open trash');
    await mount(view);

    expect(view.querySelector('a')?.getAttribute('href')).toBe('/trash');
  });

  it('is a DaisyUI hero that fills the space it is given', async () => {
    const view = await mount('opm-route-placeholder', { heading: 'x' });

    const hero = view.querySelector('.hero');
    expect(hero?.classList.contains('h-full')).toBe(true);
    expect(hero?.querySelector('.hero-content')).not.toBeNull();
  });

  it('renders text, never markup', async () => {
    const view = await mount('opm-route-placeholder', { heading: '<b>bold</b>' });

    expect(view.querySelector('h2 b')).toBeNull();
    expect(view.querySelector('h2')?.textContent).toBe('<b>bold</b>');
  });
});
