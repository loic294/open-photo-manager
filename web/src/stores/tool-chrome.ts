import { signal } from '@preact/signals-core';

/** Shared by the shell's tool buttons and the active tool view. Reset on tool entry. */
export const filtersOpen = signal(false);
export const toolHelpOpen = signal(false);
