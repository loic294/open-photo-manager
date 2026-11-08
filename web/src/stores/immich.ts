import { batch, computed, type ReadonlySignal, signal } from '@preact/signals-core';
import type { ImmichStatusDto } from '@/api/generated/ImmichStatusDto';
import type { TaskKind } from '@/api/generated/TaskKind';
import { fetchImmichStatus } from '@/api/immich';
import { events } from '@/api/sse';
import { onTaskFinished } from '@/stores/tasks';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';

const status = signal<ImmichStatusDto | null>(null);
const error = signal<string | null>(null);
export const immichStatus: ReadonlySignal<ImmichStatusDto | null> = status;
export const immichStatusError: ReadonlySignal<string | null> = error;
export const immichAttention: ReadonlySignal<boolean> = computed(() => {
  const current = status.value;
  return !!current?.connected && (current.needs_reconnect || current.needs_attention > 0);
});

let revision = 0;
let issued = 0;
let accepted = 0;
const inFlight = new Map<boolean, { promise: Promise<void>; sequence: number }>();

export function applyImmichStatus(value: ImmichStatusDto): void {
  revision += 1;
  accepted = ++issued;
  inFlight.clear();
  batch(() => {
    status.value = value;
    error.value = null;
  });
}

export function loadImmichStatus({ check = false }: { check?: boolean } = {}): Promise<void> {
  const existing = inFlight.get(check);
  if (existing) return existing.promise;
  const started = revision;
  const sequence = ++issued;
  const request = (async () => {
    try {
      const response = await fetchImmichStatus({ check });
      // A response started before a connect/save must not overwrite its newer local result.
      if (started === revision && sequence >= accepted) {
        accepted = sequence;
        batch(() => {
          status.value = response;
          error.value = null;
        });
      }
    } catch (failure) {
      if (started === revision && sequence >= accepted) {
        accepted = sequence;
        error.value = describeImmichError(failure).message;
      }
    } finally {
      if (inFlight.get(check)?.sequence === sequence) inFlight.delete(check);
    }
  })();
  inFlight.set(check, { promise: request, sequence });
  return request;
}

const REFRESH_KINDS: ReadonlySet<TaskKind> = new Set([
  'immich_sync',
  'immich_cleanup',
  'fs_move',
  'fs_trash',
  'fs_rmdir',
  'plan_execute',
  'trash_empty',
]);

export function startImmich(): () => void {
  void loadImmichStatus();
  let timer: number | undefined;
  const stops = [
    events.onResync(() => void loadImmichStatus()),
    onTaskFinished((task) => {
      if (!REFRESH_KINDS.has(task.kind)) return;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void loadImmichStatus(), 1000);
    }),
  ];
  return () => {
    window.clearTimeout(timer);
    for (const stop of stops) stop();
  };
}

/** Clears snapshots and invalidates outstanding loads between isolated tests. */
export function resetImmichForTests(): void {
  revision += 1;
  accepted = ++issued;
  inFlight.clear();
  batch(() => {
    status.value = null;
    error.value = null;
  });
}
