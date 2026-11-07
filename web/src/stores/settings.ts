import { type ReadonlySignal, signal } from '@preact/signals-core';
import { api } from '@/api/client';
import type { SettingsDto } from '@/api/generated/SettingsDto';

const current = signal<SettingsDto | null>(null);

/**
 * The server settings (`GET /api/settings`), `null` until `loadSettings()` or `saveSettings()`
 * has succeeded. It is shared by every page that needs a setting (the settings page edits it, the
 * trash page can read `trash_retention_days`). Treat it as read-only: edit a copy
 * (`{ ...settings.value, name_max_len: 143 }`) and send that with `saveSettings`.
 */
export const settings: ReadonlySignal<SettingsDto | null> = current;

let inFlight: Promise<SettingsDto> | undefined;
// Counts the saves: a load that began before one must not overwrite what the save stored.
let saves = 0;

/**
 * Fetches the settings into the store and resolves with them. Rejects with an `ApiError` (the
 * store keeps what it had); the caller decides how to show it. Calls made while a load is
 * running share it.
 */
export function loadSettings(): Promise<SettingsDto> {
  if (inFlight) return inFlight;
  const request: Promise<SettingsDto> = fetchSettings().finally(() => {
    if (inFlight === request) inFlight = undefined;
  });
  inFlight = request;
  return request;
}

async function fetchSettings(): Promise<SettingsDto> {
  const savesAtStart = saves;
  const loaded = await api.get<SettingsDto>('/api/settings');
  const newer = saves === savesAtStart ? null : current.peek();
  if (newer) return newer;
  current.value = loaded;
  return loaded;
}

/**
 * Stores the complete document (`PUT /api/settings` replaces every field at once) and resolves
 * with the normalised document the server answers with, which is also what `settings` now holds.
 * Rejects with an `ApiError`: a 422 `validation_failed` lists the rejected `fields`. Do not run two
 * saves at once.
 */
export async function saveSettings(document: SettingsDto): Promise<SettingsDto> {
  const saved = await api.put<SettingsDto>('/api/settings', { body: document });
  saves += 1;
  current.value = saved;
  return saved;
}
