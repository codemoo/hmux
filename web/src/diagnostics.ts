export type DiagnosticKind =
  | "terminal-failed"
  | "terminal-recovered"
  | "api-failed"
  | "offline"
  | "resume"
  | "runtime-error"
  | "unhandled-rejection";
type Context = { online: boolean; visible: boolean; standalone: boolean };
export type DiagnosticEvent = Context & {
  sequence: number;
  at: number;
  kind: DiagnosticKind;
  reason?: string;
  route?: string;
  code?: number;
  attempt?: number;
  retry_ms?: number;
  duration_ms?: number;
  line?: number;
  column?: number;
};
type Entry = { build: string; event: DiagnosticEvent; sent: boolean };
const kinds = new Set([
  "terminal-failed",
  "terminal-recovered",
  "api-failed",
  "offline",
  "resume",
  "runtime-error",
  "unhandled-rejection",
]);
const reasons = new Set([
  "network",
  "timeout",
  "limit",
  "unavailable",
  "output-overflow",
  "protocol",
  "http",
  "TypeError",
  "RangeError",
  "ReferenceError",
  "SyntaxError",
  "Error",
  "unknown",
]);
const routes = new Set([
  "session",
  "state",
  "action",
  "sessions",
  "account",
  "push",
  "upload",
  "other",
]);
const clientPattern =
  /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
const ttl = 24 * 60 * 60 * 1000;
export function diagnosticBuild(value: string) {
  const file = value.split("/").at(-1) || "";
  return /^app-[A-Za-z0-9_-]{1,64}\.js$/.test(file) ? file : "unknown";
}
export function diagnosticRoute(path: string) {
  const route = path.split("/")[2];
  return routes.has(route) ? route : "other";
}
export function diagnosticReason(error: unknown) {
  const name = error instanceof Error ? error.name : "unknown";
  return reasons.has(name) ? name : "unknown";
}
function fields(input: Partial<DiagnosticEvent>): Partial<DiagnosticEvent> {
  const result: Partial<DiagnosticEvent> = {};
  if (reasons.has(input.reason || "")) result.reason = input.reason;
  if (routes.has(input.route || "")) result.route = input.route;
  for (const [key, max] of [
    ["code", 4999],
    ["attempt", 1000000],
    ["retry_ms", 60000],
    ["duration_ms", 86400000],
    ["line", 10000000],
    ["column", 10000000],
  ] as const) {
    const value = input[key];
    if (typeof value === "number" && Number.isFinite(value))
      result[key] = Math.max(0, Math.min(max, Math.round(value)));
  }
  return result;
}

// A small login-owned outbox, independent of the app's API/error path. A failed
// upload must never log itself, expire a login or delay terminal reconnection.
export function createDiagnostics(options: {
  csrf: () => string;
  context: () => Context;
  build: string;
  storage?: () => Pick<Storage, "getItem" | "setItem" | "removeItem">;
  fetch?: typeof fetch;
  now?: () => number;
}) {
  const now = options.now || Date.now;
  let owner: { key: string; generation: number; csrf: string } | undefined;
  let generation = 0;
  let client = crypto.randomUUID();
  let next = 1;
  let entries: Entry[] = [];
  let timer: ReturnType<typeof setTimeout> | undefined;
  let request: AbortController | undefined;
  let retry = 10_000;
  let blocked = false;
  const build = diagnosticBuild(options.build);
  const persist = () => {
    entries = entries.filter((e) => e.event.at >= now() - ttl).slice(-100);
    if (!owner) return;
    try {
      options
        .storage?.()
        .setItem(owner.key, JSON.stringify({ client, next, entries }));
    } catch {
      /* Storage is optional. */
    }
  };
  const schedule = (delay = 10_000) => {
    if (
      !owner ||
      blocked ||
      timer !== undefined ||
      request ||
      !entries.some((e) => !e.sent)
    )
      return;
    timer = setTimeout(() => {
      timer = undefined;
      void flush();
    }, delay);
  };
  const flush = async () => {
    if (!owner || request || blocked || !options.context().online) {
      schedule(retry);
      return;
    }
    persist();
    const pending = entries.filter((e) => !e.sent);
    if (!pending.length) return;
    const firstBuild = pending[0].build;
    const batch: Entry[] = [];
    for (const entry of pending) {
      if (entry.build !== firstBuild || batch.length === 20) break;
      batch.push(entry);
    }
    const current = owner;
    const controller = new AbortController();
    request = controller;
    const deadline = setTimeout(() => controller.abort(), 5000);
    try {
      const response = await (options.fetch || fetch)("/api/diagnostics", {
        method: "POST",
        credentials: "same-origin",
        cache: "no-store",
        signal: controller.signal,
        headers: {
          "Content-Type": "application/json",
          "X-CSRF-Token": current.csrf,
        },
        body: JSON.stringify({
          version: 1,
          client,
          build: firstBuild,
          events: batch.map((e) => e.event),
        }),
      });
      if (owner !== current || controller.signal.aborted) return;
      if (response.ok) {
        for (const entry of batch) entry.sent = true;
        retry = 10_000;
        persist();
      } else if (
        response.status === 401 ||
        response.status === 403 ||
        response.status === 400
      ) {
        blocked = true; // Rebind after login; never recurse through login/error handling.
      } else {
        retry = response.status === 429 ? 60_000 : Math.min(60_000, retry * 2);
      }
    } catch {
      if (owner === current) retry = Math.min(60_000, retry * 2);
    } finally {
      clearTimeout(deadline);
      if (request === controller) request = undefined;
      if (owner === current) schedule(retry);
    }
  };
  return {
    bind(loginID: string) {
      if (!/^[A-Za-z0-9_-]{20,128}$/.test(loginID || "")) return;
      clearTimeout(timer);
      timer = undefined;
      request?.abort();
      request = undefined;
      const initial = owner ? [] : entries;
      if (owner) {
        try {
          options.storage?.().removeItem(owner.key);
        } catch {}
      }
      owner = {
        key: `hmux.diagnostics.${loginID}`,
        generation: ++generation,
        csrf: options.csrf(),
      };
      client = crypto.randomUUID();
      next = 1;
      entries = [];
      blocked = false;
      retry = 10_000;
      try {
        const raw = options.storage?.().getItem(owner.key) || "null";
        const saved = JSON.parse(raw.length <= 65536 ? raw : "null");
        if (
          saved &&
          clientPattern.test(saved.client) &&
          Number.isSafeInteger(saved.next) &&
          saved.next >= 1 &&
          saved.next <= 2147483547 &&
          Array.isArray(saved.entries)
        ) {
          client = saved.client;
          next = saved.next;
          for (const row of saved.entries.slice(-100)) {
            const e = row?.event;
            if (
              !e ||
              !kinds.has(e.kind) ||
              !Number.isSafeInteger(e.sequence) ||
              e.sequence < 1 ||
              e.sequence > 2147483547 ||
              !Number.isSafeInteger(e.at) ||
              e.at < now() - ttl ||
              e.at > now() + 300000
            )
              continue;
            entries.push({
              build: diagnosticBuild(row.build || ""),
              sent: row.sent === true,
              event: {
                ...fields(e),
                sequence: e.sequence,
                at: e.at,
                kind: e.kind,
                online: e.online === true,
                visible: e.visible === true,
                standalone: e.standalone === true,
              },
            });
            next = Math.max(next, e.sequence + 1);
          }
        }
      } catch {
        /* Corrupt or blocked browser storage cannot break startup. */
      }
      for (const row of initial)
        entries.push({ ...row, event: { ...row.event, sequence: next++ } });
      persist();
      schedule();
    },
    record(kind: DiagnosticKind, detail: Partial<DiagnosticEvent> = {}) {
      if (blocked || !kinds.has(kind)) return;
      if (next >= 2147483647) {
        client = crypto.randomUUID();
        next = 1;
        entries = [];
      }
      const context = options.context();
      const event: DiagnosticEvent = {
        ...fields(detail),
        sequence: next++,
        at: now(),
        kind,
        online: context.online === true,
        visible: context.visible === true,
        standalone: context.standalone === true,
      };
      // Collapse noisy repeated events within one second before persistence.
      const last = entries.at(-1)?.event;
      if (
        last &&
        last.kind === kind &&
        last.reason === event.reason &&
        last.code === event.code &&
        last.route === event.route &&
        event.at - last.at < 1000
      )
        return;
      entries.push({ build, event, sent: false });
      persist();
      schedule();
    },
    flush,
    snapshot: () => ({
      version: 1,
      client,
      events: entries.map((e) => ({
        build: e.build,
        ...e.event,
        pending: !e.sent,
      })),
    }),
    dispose() {
      clearTimeout(timer);
      timer = undefined;
      request?.abort();
      request = undefined;
      if (owner) {
        try {
          options.storage?.().removeItem(owner.key);
        } catch {}
      }
      owner = undefined;
      entries = [];
      client = crypto.randomUUID();
      next = 1;
      blocked = false;
    },
  };
}
