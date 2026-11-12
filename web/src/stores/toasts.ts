import { type ReadonlySignal, signal } from '@preact/signals-core';

export type ToastKind = 'success' | 'info' | 'warning' | 'error';

export interface Toast {
  readonly id: number;
  readonly kind: ToastKind;
  readonly message: string;
  /** Milliseconds on screen once not held. 0 keeps it until dismissed. */
  readonly timeout: number;
  readonly action?: ToastOptions['action'];
}

export interface ToastOptions {
  /** Milliseconds before it goes away (0 = until dismissed). Defaults depend on the kind. */
  timeout?: number;
  action?: { label: string; run: () => void };
}

/** Errors stay longest: they usually need to be read, and often copied. */
export const DEFAULT_TIMEOUT_MS: Readonly<Record<ToastKind, number>> = {
  success: 4000,
  info: 4000,
  warning: 6000,
  error: 10000,
};

/** The oldest toast is dropped when more than this many are showing. */
export const MAX_TOASTS = 5;

const list = signal<readonly Toast[]>([]);

/** The toasts on screen, oldest first. Render them with `<opm-toast-host>`. */
export const toasts: ReadonlySignal<readonly Toast[]> = list;

const timers = new Map<number, number>();
const held = new Set<number>();
let lastId = 0;

function stopTimer(id: number): void {
  const handle = timers.get(id);
  if (handle !== undefined) {
    window.clearTimeout(handle);
    timers.delete(id);
  }
}

function startTimer(toast: Toast): void {
  stopTimer(toast.id);
  if (toast.timeout > 0 && !held.has(toast.id)) {
    timers.set(
      toast.id,
      window.setTimeout(() => dismissToast(toast.id), toast.timeout),
    );
  }
}

/**
 * Shows a toast and returns its id. Pushing the same kind and message while it is still showing
 * does not stack a copy: the existing toast stays and its timer restarts.
 */
export function pushToast(kind: ToastKind, message: string, options: ToastOptions = {}): number {
  const requested = Math.max(0, options.timeout ?? DEFAULT_TIMEOUT_MS[kind]);
  const timeout = options.action && requested !== 0 ? Math.max(10_000, requested) : requested;
  const existing = list.peek().find((item) => item.kind === kind && item.message === message);
  if (existing) {
    const updated: Toast = { ...existing, timeout, action: options.action };
    list.value = list.peek().map((item) => (item.id === existing.id ? updated : item));
    startTimer(updated);
    return existing.id;
  }

  const toast: Toast = { id: ++lastId, kind, message, timeout, action: options.action };
  const next = [...list.peek(), toast];
  for (const dropped of next.splice(0, Math.max(0, next.length - MAX_TOASTS))) {
    forget(dropped.id);
  }
  list.value = next;
  startTimer(toast);
  return toast.id;
}

function forget(id: number): void {
  stopTimer(id);
  held.delete(id);
}

export function dismissToast(id: number): void {
  forget(id);
  if (list.peek().some((item) => item.id === id)) {
    list.value = list.peek().filter((item) => item.id !== id);
  }
}

export function clearToasts(): void {
  for (const { id } of list.peek()) forget(id);
  list.value = [];
}

/** Pauses a toast's timer (the pointer or keyboard focus is on it, so it is being read). */
export function holdToast(id: number): void {
  if (!list.peek().some((item) => item.id === id)) return;
  held.add(id);
  stopTimer(id);
}

/** Starts the full timeout again after `holdToast`. */
export function releaseToast(id: number): void {
  held.delete(id);
  const toast = list.peek().find((item) => item.id === id);
  if (toast) startTimer(toast);
}
