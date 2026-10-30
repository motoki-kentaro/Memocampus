// Durable delivery queue for visit and tab-snapshot events, kept in chrome.storage.local.
//
// - Every storage update runs through one exclusive chain, so concurrent enqueue/flush calls in
//   the service worker cannot overwrite each other's read-modify-write.
// - Only one flush runs at a time; a flush requested meanwhile makes the running one loop again.
// - An event the host rejects permanently is copied to the dead-letter queue first and removed
//   from the pending queue only after that copy is stored. If the copy cannot be stored, the event
//   stays at the head and delivery pauses (no silent loss).
// - Every other failure (host unreachable, timeout, no vault, busy vault, unknown codes) is
//   temporary: the event stays queued and is retried on the next flush. The host stores events
//   with INSERT OR IGNORE by id, so a resend after a lost ACK never duplicates rows.
// - A visit or snapshot not declared normal (`incognito: false`) is removed without a dead-letter
//   copy, so its URL and title are not kept anywhere. The service worker never queues one; this
//   covers events queued by an older version and a refusal by the host.

export const QUEUE_KEY = "pendingVisitEvents";
export const DEAD_LETTER_KEY = "deadLetterEvents";
export const STATS_KEY = "queueStats";
export const MAX_PENDING = 5000;
export const MAX_DEAD_LETTERS = 1000;
export const SEND_TIMEOUT_MS = 30_000;

/** Host error codes meaning "this exact event can never be stored". */
export const PERMANENT_ERRORS = new Set(["invalid_url", "invalid_event", "invalid_message", "unknown_type"]);

/** The host's refusal of an event not declared normal: discarded, never quarantined. */
export const INCOGNITO_ERROR = "incognito_not_allowed";

export class QueueError extends Error {
  constructor(code) {
    super(code);
    this.name = "QueueError";
    this.code = code;
  }
}

/**
 * Classifies one host response for `event`. Chrome pairs each sendNativeMessage call with its own
 * response, so a permanent rejection carrying `eventId: null` (the event had no usable id) still
 * belongs to this event.
 */
export function classifyResponse(event, response) {
  if (response?.ok === true && response.eventId === event?.id) return { kind: "delivered" };
  if (response?.ok === false && typeof response.error === "string") {
    const sameEvent = response.eventId === event?.id || response.eventId === null;
    if (response.error === INCOGNITO_ERROR && sameEvent) return { kind: "discard", code: response.error };
    if (PERMANENT_ERRORS.has(response.error) && sameEvent) return { kind: "permanent", code: response.error };
    return { kind: "temporary", code: response.error };
  }
  return { kind: "temporary", code: "invalid_ack" };
}

const isUndeclared = (event) => (event.type === "visit" || event.type === "tab_snapshot") && event.incognito !== false;

const hasId = (event) => typeof event?.id === "string" && event.id !== "";

function sameEvent(a, b) {
  return hasId(b) ? a?.id === b.id : JSON.stringify(a) === JSON.stringify(b);
}

function withTimeout(promise, timeoutMs) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new QueueError("timeout")), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

export function createEventQueue({ storage, send, now = () => new Date(), timeoutMs = SEND_TIMEOUT_MS, onChange = () => {} }) {
  let tail = Promise.resolve();
  let flushing = null;
  let flushAgain = false;
  let lastBlockedBy = null;

  /** Runs `task` after every previously scheduled storage update has settled. */
  function exclusive(task) {
    const run = tail.then(task);
    tail = run.then(
      () => {},
      () => {},
    );
    return run;
  }

  async function readList(key) {
    const stored = await storage.get(key);
    return Array.isArray(stored?.[key]) ? stored[key] : [];
  }

  /** Appends all events or none: the whole batch is written with a single storage call. */
  function enqueueAll(events) {
    return exclusive(async () => {
      const queue = await readList(QUEUE_KEY);
      const known = new Set(queue.filter(hasId).map((event) => event.id));
      const fresh = [];
      for (const event of events) {
        if (hasId(event) && known.has(event.id)) continue;
        if (hasId(event)) known.add(event.id);
        fresh.push(event);
      }
      if (queue.length + fresh.length > MAX_PENDING) throw new QueueError("queue_full");
      if (fresh.length === 0) return 0;
      try {
        await storage.set({ [QUEUE_KEY]: [...queue, ...fresh] });
      } catch {
        throw new QueueError("storage_write_failed");
      }
      return fresh.length;
    });
  }

  /** Counts an event that could not even be queued (full queue or storage failure). */
  function recordDropped() {
    return exclusive(async () => {
      const stored = await storage.get(STATS_KEY);
      const stats = stored?.[STATS_KEY] && typeof stored[STATS_KEY] === "object" ? stored[STATS_KEY] : {};
      await storage.set({ [STATS_KEY]: { ...stats, dropped: (Number(stats.dropped) || 0) + 1, lastDroppedAt: now().toISOString() } });
    }).finally(onChange);
  }

  function remove(event) {
    return exclusive(async () => {
      const queue = await readList(QUEUE_KEY);
      await storage.set({ [QUEUE_KEY]: queue.filter((item) => !sameEvent(item, event)) });
    });
  }

  function quarantine(event, errorCode) {
    return exclusive(async () => {
      const dead = await readList(DEAD_LETTER_KEY);
      const eventId = hasId(event) ? event.id : null;
      // A crash between the two writes below leaves the event in both lists; the retry finds
      // it already quarantined and only removes it from the queue.
      const already = eventId !== null && dead.some((entry) => entry?.eventId === eventId);
      if (!already) {
        if (dead.length >= MAX_DEAD_LETTERS) throw new QueueError("dead_letter_full");
        try {
          await storage.set({ [DEAD_LETTER_KEY]: [...dead, { errorCode, eventId, failedAt: now().toISOString(), event }] });
        } catch {
          throw new QueueError("dead_letter_write_failed");
        }
      }
      const queue = await readList(QUEUE_KEY);
      await storage.set({ [QUEUE_KEY]: queue.filter((item) => !sameEvent(item, event)) });
    });
  }

  async function deliver(event) {
    if (!event || typeof event !== "object" || Array.isArray(event)) return { kind: "permanent", code: "invalid_event" };
    if (isUndeclared(event)) return { kind: "discard", code: INCOGNITO_ERROR };
    try {
      return classifyResponse(event, await withTimeout(Promise.resolve().then(() => send(event)), timeoutMs));
    } catch (error) {
      return { kind: "temporary", code: typeof error?.code === "string" ? error.code : "host_unavailable" };
    }
  }

  /** Sends from the head until the queue is empty or delivery has to pause. */
  async function drain() {
    for (;;) {
      const [head] = await readList(QUEUE_KEY);
      if (head === undefined) return null;
      const outcome = await deliver(head);
      try {
        if (outcome.kind === "delivered" || outcome.kind === "discard") await remove(head);
        else if (outcome.kind === "permanent") await quarantine(head, outcome.code);
        else return outcome.code;
      } catch (error) {
        return error instanceof QueueError ? error.code : "storage_write_failed";
      }
    }
  }

  /** Resolves to the code that paused delivery, or null when the queue was emptied. */
  function flush() {
    if (flushing) {
      flushAgain = true;
      return flushing;
    }
    flushing = (async () => {
      let blockedBy;
      do {
        flushAgain = false;
        blockedBy = await drain();
      } while (flushAgain && blockedBy === null);
      lastBlockedBy = blockedBy;
      return blockedBy;
    })().finally(() => {
      flushing = null;
      onChange();
    });
    return flushing;
  }

  async function status() {
    const stored = await storage.get([QUEUE_KEY, DEAD_LETTER_KEY, STATS_KEY]);
    const list = (key) => (Array.isArray(stored?.[key]) ? stored[key] : []);
    const dead = list(DEAD_LETTER_KEY);
    return {
      pending: list(QUEUE_KEY).length,
      deadLetters: dead.length,
      deadLetterCodes: [...new Set(dead.map((entry) => entry?.errorCode).filter(Boolean))],
      dropped: Number(stored?.[STATS_KEY]?.dropped) || 0,
      blockedBy: lastBlockedBy,
    };
  }

  /** Where the given event ids currently are: still pending, quarantined, or neither (stored). */
  async function locate(ids) {
    const stored = await storage.get([QUEUE_KEY, DEAD_LETTER_KEY]);
    const pendingIds = new Set((Array.isArray(stored?.[QUEUE_KEY]) ? stored[QUEUE_KEY] : []).map((event) => event?.id));
    const deadIds = new Set((Array.isArray(stored?.[DEAD_LETTER_KEY]) ? stored[DEAD_LETTER_KEY] : []).map((entry) => entry?.eventId));
    let pending = 0;
    let deadLettered = 0;
    for (const id of ids) {
      if (pendingIds.has(id)) pending += 1;
      else if (deadIds.has(id)) deadLettered += 1;
    }
    return { pending, deadLettered, stored: ids.length - pending - deadLettered };
  }

  return { enqueue: (event) => enqueueAll([event]), enqueueAll, recordDropped, flush, status, locate };
}
