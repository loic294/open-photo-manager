import {
  MAX_LIMIT,
  type MockControl,
  type MockState,
  runAddAssets,
  runGenerate,
} from './control.ts';
import { parseFailRule } from './faults.ts';
import { ApiError, badRequest, ok, type Reply } from './http.ts';
import type { MockContext } from './mock.ts';
import { createChecker } from './validate.ts';

interface ControlInput {
  query: URLSearchParams;
  body: unknown;
}

type Handler = (input: ControlInput) => Reply | Promise<Reply>;

export interface ControlRouter {
  dispatch(method: string, path: string, query: URLSearchParams, body: unknown): Promise<Reply>;
}

const DEFAULT_WAIT_MS = 10_000;

function intParam(query: URLSearchParams, name: string, min: number, max: number) {
  const raw = query.get(name);
  if (raw === null) {
    return undefined;
  }
  const value = Number(raw);
  if (raw.trim() === '' || !Number.isInteger(value) || value < min || value > max) {
    throw badRequest(`${name} must be an integer between ${min} and ${max}`);
  }
  return value;
}

/** The HTTP face of `state` and `control`: everything under `/__mock/`. */
export function createControlRouter(
  ctx: MockContext,
  control: MockControl,
  state: MockState,
): ControlRouter {
  const handlers = new Map<string, Handler>(
    Object.entries<Handler>({
      'GET /__mock/state': ({ query }) =>
        ok(
          state.snapshot({
            path: query.get('path') ?? undefined,
            limit: intParam(query, 'limit', 0, MAX_LIMIT),
            offset: intParam(query, 'offset', 0, Number.MAX_SAFE_INTEGER),
          }),
        ),

      'GET /__mock/log': ({ query }) =>
        ok(state.requests(intParam(query, 'since', 0, Number.MAX_SAFE_INTEGER))),

      'POST /__mock/reset': () => {
        control.reset();
        return ok({ reset: true });
      },

      'POST /__mock/scan': async ({ body }) => {
        const c = createChecker(body ?? {});
        c.only(['libraryId']);
        const libraryId = c.string('libraryId', { nonEmpty: true });
        c.done();
        return ok({ jobs: await control.scan(libraryId) });
      },

      'POST /__mock/assets': ({ body }) => {
        const c = createChecker(body);
        c.only(['assets']);
        c.done();
        if (ctx.readOnlyBacking?.()) throw new ApiError(503, 'backing store is read-only');
        return ok(runAddAssets(ctx, c.obj.assets));
      },

      'POST /__mock/generate': ({ body }) => {
        if (ctx.readOnlyBacking?.()) throw new ApiError(503, 'backing store is read-only');
        return ok(runGenerate(ctx, body));
      },

      'POST /__mock/latency': ({ body }) => {
        const c = createChecker(body);
        c.only(['ms']);
        const ms = c.integer('ms', { required: true, min: 0 });
        c.done();
        control.latency(ms ?? 0);
        return ok({ latencyMs: ctx.settings.latencyMs });
      },

      'POST /__mock/revoke-key': ({ body }) => {
        const c = createChecker(body ?? {});
        c.only(['name']);
        const name = c.string('name', { nonEmpty: true });
        c.done();
        return ok({ revoked: control.revokeKey(name) });
      },

      'POST /__mock/fail': ({ body }) => ok(ctx.faults.addRule(parseFailRule(body))),
      'GET /__mock/fail': () => ok({ rules: control.failures() }),
      'DELETE /__mock/fail': () => {
        control.clearFailures();
        return ok({ cleared: true });
      },

      'POST /__mock/gate': ({ body }) => {
        const c = createChecker(body);
        c.only(['match', 'times']);
        const match = c.string('match', { required: true, nonEmpty: true });
        const times = c.integer('times', { min: 1 });
        c.done();
        return ok(control.gate(match ?? '', { times }));
      },

      // `?waitHeld=N` answers once N requests are held (a 408 after `timeoutMs`).
      'GET /__mock/gate': async ({ query }) => {
        const waitHeld = intParam(query, 'waitHeld', 1, 10_000);
        if (waitHeld !== undefined) {
          const timeoutMs = intParam(query, 'timeoutMs', 0, 600_000) ?? DEFAULT_WAIT_MS;
          try {
            await control.waitForHeld(waitHeld, timeoutMs);
          } catch (error) {
            throw new ApiError(408, error instanceof Error ? error.message : String(error));
          }
        }
        return ok({ gates: ctx.faults.gates(), held: ctx.faults.held() });
      },

      'POST /__mock/gate/release': () => ok({ released: control.releaseGate() }),
    }),
  );

  return {
    async dispatch(method, path, query, body) {
      const handler = handlers.get(`${method} ${path.replace(/\/+$/, '')}`);
      if (handler === undefined) {
        throw new ApiError(404, `Cannot ${method} ${path}`);
      }
      return handler({ query, body });
    },
  };
}
