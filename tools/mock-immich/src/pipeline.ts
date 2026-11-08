import type { IncomingMessage, ServerResponse } from 'node:http';
import type { ControlRouter } from './control-http.ts';
import { ApiError, parseJson, readBody, sendError, sendJson } from './http.ts';
import type { MockContext } from './mock.ts';
import { summariseBody } from './requestlog.ts';
import type { Router } from './router.ts';
import { sleep } from './util.ts';

export type RequestHandler = (req: IncomingMessage, res: ServerResponse) => Promise<void>;

function failure(error: unknown): ApiError {
  if (error instanceof ApiError) {
    return error;
  }
  if (error instanceof Error && error.name === 'AbortError') {
    return new ApiError(503, 'the mock is stopping');
  }
  const detail = error instanceof Error ? (error.stack ?? error.message) : String(error);
  process.stderr.write(`mock-immich: unexpected error: ${detail}\n`);
  return new ApiError(500, 'Internal server error');
}

/**
 * Every request goes through the same steps: read and parse the body (a malformed one is a 400
 * before anything else, as in NestJS), note it in the log, wait for the latency, apply a failure
 * rule, wait at a gate, then route, authenticate and handle. `/__mock/*` skips all of this but
 * the body: it is the test's own door, not part of the traffic under test.
 */
export function createRequestHandler(
  ctx: MockContext,
  router: Router,
  control: ControlRouter,
): RequestHandler {
  return async (req, res) => {
    const method = req.method ?? 'GET';
    const url = new URL(req.url ?? '/', 'http://mock-immich.invalid');
    const path = url.pathname;

    if (path.startsWith('/__mock/')) {
      try {
        const body = parseJson(await readBody(req));
        const reply = await control.dispatch(method, path, url.searchParams, body);
        sendJson(res, reply.status, reply.body);
      } catch (error) {
        sendError(res, failure(error));
      }
      return;
    }

    const handle = ctx.log.begin(method, path, url.search.slice(1));
    const { entry } = handle;
    try {
      const body = parseJson(await readBody(req));
      if (body !== undefined) {
        entry.bodySummary = summariseBody(body);
      }
      if (ctx.settings.latencyMs > 0) {
        await sleep(ctx.settings.latencyMs, ctx.stopSignal);
      }

      const decision = ctx.faults.decide(method, path);
      if (decision.kind === 'drop') {
        entry.injected = 'drop';
        handle.done(0);
        res.destroy();
        return;
      }
      if (decision.kind === 'status') {
        entry.injected = 'fail';
        throw new ApiError(decision.status, 'failure injected by the mock');
      }
      if (decision.kind === 'slow') {
        entry.injected = 'slow';
        await sleep(decision.delayMs, ctx.stopSignal);
      }

      const held = await ctx.faults.hold(method, path, res, () => {
        entry.injected ??= 'gate';
      });
      if (held === 'skip') {
        handle.done(0);
        return;
      }

      if (
        ctx.beforeAssetRead &&
        ((method === 'GET' &&
          (path.startsWith('/api/assets/') || path.startsWith('/api/libraries'))) ||
          (method === 'POST' && path === '/api/search/metadata'))
      ) {
        await ctx.beforeAssetRead();
      }
      const reply = await router.dispatch(ctx, req, path, url.searchParams, body, entry);
      handle.done(reply.status);
      sendJson(res, reply.status, reply.body);
    } catch (error) {
      const problem = failure(error);
      handle.done(problem.status);
      sendError(res, problem);
    }
  };
}
