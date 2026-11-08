import { fileURLToPath } from 'node:url';

export { type MockImmich, type MockImmichOptions, startMockImmich } from './src/app.ts';
export type {
  AddedAssets,
  AlbumView,
  AssetOptions,
  GenerateOptions,
  KeyView,
  MockControl,
  MockState,
  StateQuery,
  StateSnapshot,
  TagView,
  UserView,
} from './src/control.ts';
export type { FailRuleInput, FailRuleView, GateView, HeldView } from './src/faults.ts';
export type { Settings } from './src/mock.ts';
export type { LogEntry } from './src/requestlog.ts';
export { ScenarioError } from './src/scenario.ts';
export type { Job, Mount, Scenario, Violation } from './src/types.ts';

const scenarioPath = (name: string) =>
  fileURLToPath(new URL(`./scenarios/${name}.json`, import.meta.url));

/** The scenarios that ship with the mock, as absolute paths (`startMockImmich({ scenario })`). */
export const scenarioFiles = {
  default: scenarioPath('default'),
  limited: scenarioPath('limited'),
  fixtures: scenarioPath('fixtures'),
} as const;

/** The test accounts of the shipped scenarios (obviously fake). */
export const testUsers = {
  admin: { email: 'admin@example.test', password: 'admin-password', name: 'Admin' },
  user: { email: 'user@example.test', password: 'user-password', name: 'Regular User' },
} as const;
