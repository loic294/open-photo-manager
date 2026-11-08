import type { ServerResponse } from 'node:http';
import { ApiError } from './http.ts';
import { createChecker } from './validate.ts';

export interface FailRuleInput {
  /** `[METHOD ]/api/path`, where `*` stands for any text: `PUT /api/assets`, `/api/search/*`. */
  match: string;
  /** An HTTP error status, `'drop'` (close the connection) or `'slow'` (answer late, normally). */
  status: number | 'drop' | 'slow';
  /** How many matching requests fail (all of them when absent). */
  times?: number;
  /** How many matching requests pass before the failures start. */
  after?: number;
  /** The delay of `'slow'`. */
  delayMs?: number;
}

export interface FailRuleView {
  id: number;
  match: string;
  status: number | 'drop' | 'slow';
  times: number | null;
  after: number;
  delayMs: number;
  /** Matching requests so far. */
  seen: number;
  /** Requests the rule failed so far. */
  fired: number;
}

export interface GateView {
  id: number;
  match: string;
  times: number | null;
  held: number;
}

export interface HeldView {
  id: number;
  method: string;
  path: string;
  since: string;
}

/** What the pipeline does with a request before it reaches its handler. */
export type Decision =
  | { kind: 'pass' }
  | { kind: 'status'; status: number }
  | { kind: 'drop' }
  | { kind: 'slow'; delayMs: number };

interface Matcher {
  method: string | null;
  regex: RegExp;
}

const MATCH_RE = /^(?:([A-Z]+) )?(\/\S*)$/;
const DEFAULT_SLOW_MS = 5000;

function compileMatch(match: string): Matcher {
  const parsed = MATCH_RE.exec(match.trim());
  if (parsed === null) {
    throw new ApiError(400, [
      `match must look like "[METHOD ]/api/path*", got ${JSON.stringify(match)}`,
    ]);
  }
  const source = (parsed[2] ?? '')
    .split('*')
    .map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
    .join('.*');
  return { method: parsed[1] ?? null, regex: new RegExp(`^${source}$`, 's') };
}

const matches = (matcher: Matcher, method: string, path: string): boolean =>
  (matcher.method === null || matcher.method === method) && matcher.regex.test(path);

/** Reads and checks the body of `POST /__mock/fail`. */
export function parseFailRule(body: unknown): FailRuleInput {
  const c = createChecker(body);
  c.only(['match', 'status', 'times', 'after', 'delayMs']);
  const match = c.string('match', { required: true, nonEmpty: true });
  const rawStatus = c.obj.status;
  let status: FailRuleInput['status'] = 'drop';
  if (rawStatus === 'drop' || rawStatus === 'slow') {
    status = rawStatus;
  } else if (typeof rawStatus === 'number' && Number.isInteger(rawStatus)) {
    status = rawStatus;
    if (rawStatus < 400 || rawStatus > 599) {
      c.fail('status', 'must be between 400 and 599, or "drop", or "slow"');
    }
  } else {
    c.fail('status', 'must be an HTTP error status (400-599), "drop" or "slow"');
  }
  const times = c.integer('times', { min: 1 });
  const after = c.integer('after', { min: 0 });
  const delayMs = c.integer('delayMs', { min: 0 });
  c.done();
  if (match === undefined) {
    throw new ApiError(400, ['match must be a string']);
  }
  compileMatch(match);
  return {
    match,
    status,
    ...(times === undefined ? {} : { times }),
    ...(after === undefined ? {} : { after }),
    ...(delayMs === undefined ? {} : { delayMs }),
  };
}

interface FailRule extends FailRuleView {
  matcher: Matcher;
}

interface GateRule extends GateView {
  matcher: Matcher;
}

interface Held extends HeldView {
  release: () => void;
}

export interface Faults {
  addRule(input: FailRuleInput): FailRuleView;
  rules(): FailRuleView[];
  clearRules(): void;
  /** Decides, and counts the request against the matching rules. */
  decide(method: string, path: string): Decision;
  addGate(match: string, times?: number): GateView;
  gates(): GateView[];
  held(): HeldView[];
  /**
   * Holds the request while a gate matches it: `'pass'` when none did, `'released'` once the gate
   * let it go, `'skip'` when the client went away meanwhile. `onHold` runs as the request starts
   * to wait, so the log can show it.
   */
  hold(
    method: string,
    path: string,
    res: ServerResponse,
    onHold?: () => void,
  ): Promise<'pass' | 'released' | 'skip'>;
  /** Lets every held request go and removes the gates; returns how many were held. */
  releaseGates(): number;
  waitForHeld(count: number, timeoutMs: number): Promise<HeldView[]>;
  /** Forgets every rule and gate and releases what is held. */
  reset(): void;
}

export function createFaults(): Faults {
  let rules: FailRule[] = [];
  let gates: GateRule[] = [];
  let held: Held[] = [];
  let waiters: Array<() => void> = [];
  let nextId = 1;

  const strip = ({ matcher: _matcher, ...view }: FailRule): FailRuleView => view;
  const heldView = ({ release: _release, ...view }: Held): HeldView => view;
  const notify = () => {
    for (const waiter of [...waiters]) {
      waiter();
    }
  };

  const faults: Faults = {
    addRule(input) {
      const rule: FailRule = {
        id: nextId++,
        match: input.match,
        status: input.status,
        times: input.times ?? null,
        after: input.after ?? 0,
        delayMs: input.delayMs ?? DEFAULT_SLOW_MS,
        seen: 0,
        fired: 0,
        matcher: compileMatch(input.match),
      };
      rules.push(rule);
      return strip(rule);
    },
    rules: () => rules.map(strip),
    clearRules() {
      rules = [];
    },
    decide(method, path) {
      for (const rule of rules) {
        if (!matches(rule.matcher, method, path)) {
          continue;
        }
        rule.seen += 1;
        const position = rule.seen - rule.after;
        if (position < 1 || (rule.times !== null && position > rule.times)) {
          continue;
        }
        rule.fired += 1;
        if (rule.status === 'drop') {
          return { kind: 'drop' };
        }
        if (rule.status === 'slow') {
          return { kind: 'slow', delayMs: rule.delayMs };
        }
        return { kind: 'status', status: rule.status };
      }
      return { kind: 'pass' };
    },
    addGate(match, times) {
      const gate: GateRule = {
        id: nextId++,
        match,
        times: times ?? null,
        held: 0,
        matcher: compileMatch(match),
      };
      gates.push(gate);
      return { id: gate.id, match: gate.match, times: gate.times, held: 0 };
    },
    gates: () => gates.map(({ matcher: _matcher, ...view }) => view),
    held: () => held.map(heldView),
    hold(method, path, res, onHold) {
      const gate = gates.find(
        (candidate) =>
          matches(candidate.matcher, method, path) &&
          (candidate.times === null || candidate.held < candidate.times),
      );
      if (gate === undefined) {
        return Promise.resolve('pass');
      }
      gate.held += 1;
      onHold?.();
      return new Promise((resolve) => {
        const entry: Held = {
          id: nextId++,
          method,
          path,
          since: new Date().toISOString(),
          release: () => {
            res.off('close', onClose);
            held = held.filter((other) => other !== entry);
            resolve('released');
          },
        };
        const onClose = () => {
          held = held.filter((other) => other !== entry);
          resolve('skip');
          notify();
        };
        res.once('close', onClose);
        held.push(entry);
        notify();
      });
    },
    releaseGates() {
      const count = held.length;
      gates = [];
      for (const entry of [...held]) {
        entry.release();
      }
      return count;
    },
    waitForHeld(count, timeoutMs) {
      return new Promise((resolve, reject) => {
        const check = () => {
          if (held.length >= count) {
            clearTimeout(timer);
            waiters = waiters.filter((waiter) => waiter !== check);
            resolve(held.map(heldView));
          }
        };
        const timer = setTimeout(() => {
          waiters = waiters.filter((waiter) => waiter !== check);
          reject(
            new Error(`timed out waiting for ${count} held request(s); ${held.length} are held`),
          );
        }, timeoutMs);
        waiters.push(check);
        check();
      });
    },
    reset() {
      faults.clearRules();
      faults.releaseGates();
    },
  };
  return faults;
}
