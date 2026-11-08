import { computed, type ReadonlySignal, signal } from '@preact/signals-core';
import { api } from '@/api/client';
import { errorMessage } from '@/api/errors';
import type { TaskState } from '@/api/generated/TaskState';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { events } from '@/api/sse';

/**
 * `idle`: never loaded. `loading`: the first load (or a retry after a failed first load) is in
 * flight. `ready`: there is a list, possibly stale if `volumesError` is set. `error`: the first
 * load failed and there is no list.
 */
export type VolumesStatus = 'idle' | 'loading' | 'ready' | 'error';

const list = signal<readonly VolumeDto[]>([]);
const status = signal<VolumesStatus>('idle');
const error = signal<string | undefined>(undefined);

/** The mounted volumes in server order. Empty until the first load succeeds. */
export const volumes: ReadonlySignal<readonly VolumeDto[]> = list;
export const volumesStatus: ReadonlySignal<VolumesStatus> = status;
/** Message of the last failed load; cleared by the next successful one. */
export const volumesError: ReadonlySignal<string | undefined> = error;

const byName: ReadonlySignal<ReadonlyMap<string, VolumeDto>> = computed(
  () => new Map(list.value.map((volume) => [volume.name, volume])),
);

/** Looks a volume up by its name, the identity used in URLs. Reactive inside `render()`. */
export function volumeByName(name: string | null | undefined): VolumeDto | undefined {
  return name == null ? undefined : byName.value.get(name);
}

/** The API identifies volumes by id, the URL by name: this translates. */
export function volumeIdByName(name: string | null | undefined): number | undefined {
  return volumeByName(name)?.id;
}

function sameVolumes(a: readonly VolumeDto[], b: readonly VolumeDto[]): boolean {
  return (
    a.length === b.length &&
    a.every((volume, index) => {
      const other = b[index];
      return (
        other !== undefined &&
        volume.id === other.id &&
        volume.name === other.name &&
        volume.writable === other.writable &&
        volume.online === other.online &&
        volume.fs_type === other.fs_type &&
        volume.case_insensitive === other.case_insensitive &&
        volume.watch_mode === other.watch_mode &&
        volume.watch_reason === other.watch_reason
      );
    })
  );
}

async function fetchOnce(): Promise<void> {
  if (status.peek() !== 'ready') status.value = 'loading';
  try {
    const response = await api.get<readonly VolumeDto[]>('/api/volumes');
    if (!Array.isArray(response)) throw new Error('Unexpected response from /api/volumes');
    // An unchanged list keeps its identity, so nothing re-renders for a no-op refresh.
    if (!sameVolumes(list.peek(), response)) list.value = response;
    error.value = undefined;
    status.value = 'ready';
  } catch (failure) {
    error.value = errorMessage(failure, 'Could not load the volumes');
    if (status.peek() !== 'ready') status.value = 'error';
  }
}

let inFlight: Promise<void> | undefined;
let reloadRequested = false;

/**
 * Fetches `GET /api/volumes` into the store. Never rejects: a failure lands in `volumesError`
 * and the previous list stays. Calls made while a load is running share it, and cause exactly
 * one more load afterwards so the result is never older than the call.
 */
export function loadVolumes(): Promise<void> {
  if (inFlight) {
    reloadRequested = true;
    return inFlight;
  }
  inFlight = (async () => {
    try {
      do {
        reloadRequested = false;
        await fetchOnce();
      } while (reloadRequested);
    } finally {
      inFlight = undefined;
    }
  })();
  return inFlight;
}

// A record, so a state added to the backend enum fails the type check until it is classified here.
const IS_TERMINAL: Readonly<Record<TaskState, boolean>> = {
  queued: false,
  running: false,
  done: true,
  failed: true,
  cancelled: true,
  interrupted: true,
};

/**
 * Loads the volumes now and keeps them fresh: after the event stream reconnects (events may
 * have been missed) and whenever an `index` task changes state, a task seen for the first time
 * included. Progress-only updates of a task do not refresh. The shell calls this once; the
 * returned function stops it.
 */
export function startVolumes(): () => void {
  void loadVolumes();
  // The last state seen per index task; an id is forgotten once its state is terminal.
  const indexStates = new Map<number, TaskState>();
  const stops = [
    events.onResync(() => void loadVolumes()),
    events.on('task', ({ task }) => {
      if (task.kind !== 'index') return;
      const changed = indexStates.get(task.id) !== task.state;
      if (IS_TERMINAL[task.state]) indexStates.delete(task.id);
      else indexStates.set(task.id, task.state);
      if (changed) void loadVolumes();
    }),
  ];
  return () => {
    for (const stop of stops) stop();
  };
}

/** Back to the initial state, for tests. */
export function resetVolumesForTests(): void {
  list.value = [];
  status.value = 'idle';
  error.value = undefined;
}
