import type { ReactiveController } from 'lit';
import { expect, it, vi } from 'vitest';
import { UnsavedGuard } from '@/controllers/unsaved-guard';

it('guards only a connected, dirty host and always removes its listener', () => {
  let dirty = false;
  const add = vi.spyOn(window, 'addEventListener');
  const remove = vi.spyOn(window, 'removeEventListener');
  const host = {
    addController: vi.fn<(controller: ReactiveController) => void>(),
    removeController: vi.fn(),
    requestUpdate: vi.fn(),
    updateComplete: Promise.resolve(true),
  };
  const guard = new UnsavedGuard(host, () => dirty);
  expect(host.addController).toHaveBeenCalledWith(guard);
  guard.hostConnected();
  expect(add).not.toHaveBeenCalled();
  dirty = true;
  guard.sync();
  expect(add).toHaveBeenCalledOnce();
  const event = new Event('beforeunload', { cancelable: true });
  window.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(true);
  guard.sync();
  expect(add).toHaveBeenCalledOnce();
  dirty = false;
  guard.sync();
  expect(remove).toHaveBeenCalledOnce();
  dirty = true;
  guard.sync();
  guard.hostDisconnected();
  expect(remove).toHaveBeenCalledTimes(2);
  guard.sync();
  expect(add).toHaveBeenCalledTimes(2);
  guard.hostConnected();
  expect(add).toHaveBeenCalledTimes(3);
  guard.hostDisconnected();
  vi.restoreAllMocks();
});
