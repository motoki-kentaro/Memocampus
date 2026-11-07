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
// - Only events the host allowed are ever enqueued, and what is stored is not trusted later. Every
//   flush asks the policy first. Without a valid policy nothing is sent and nothing is removed.
//   With one, whenever its version changes, the host judges every stored URL, and those it refuses
//   are removed from the pending and dead-letter lists without a copy. Without a complete answer
//   nothing is sent. A visit that stays loses a `navigation.sourceVisitId` naming a removed event (its
//   id and place are kept). The host judges each event again on arrival and acknowledges a refused one as
//   dropped, so no event needs a separate check here and none can block those behind it.
// - A visit or snapshot not declared normal (`incognito: false`) is removed without a dead-letter
//   copy, so its URL and title are not kept anywhere. The service worker never queues one; this
//   covers events queued by an older version and a refusal by the host.
// - A visit with a `navigation` (Issue #41) is sent as queued, with the same id, source and facts on
//   every resend. It is sent with its navigation only to a host that names `navigation_v1` (asked
//   once per delivery run, `capabilities`). It counts as delivered with its navigation only if the
//   host answers `navigation: "stored"`. The visit itself is stored in every other case below, so a
//   resend could not store the navigation either: the event leaves the queue and the loss is counted
//   (`navigationInvalid`, `navigationNotStored`), shown like the dropped count, never silent:
//     `navigation: "invalid"`                  -> navigationInvalid
//     no `navigation` in the ACK               -> navigationNotStored (the host is asked again)
//     host without `navigation_v1`             -> sent without it, navigationNotStored
//   No URL is copied anywhere for these counts. An answer with any other `navigation` value is an
//   invalid ACK (temporary), as before.

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

/** Navigation outcomes in a host ACK (Slice 1 contract). */
export const NAVIGATION_STORED = "stored";
export const NAVIGATION_INVALID = "invalid";

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
  if (response?.ok === true && response.dropped === "policy" && (response.eventId === event?.id || response.eventId === null)) return { kind: "discard", code: "dropped_by_policy" };
  if (response?.ok === true && response.dropped === undefined && response.eventId === event?.id) {
    if (!carriesNavigation(event)) return { kind: "delivered" };
    if (response.navigation === NAVIGATION_STORED) return { kind: "delivered" };
    if (response.navigation === NAVIGATION_INVALID) return { kind: "delivered", lost: "navigationInvalid" };
    if (response.navigation === undefined) return { kind: "delivered", lost: "navigationNotStored" };
    return { kind: "temporary", code: "invalid_ack" };
  }
  if (response?.ok === false && typeof response.error === "string") {
    const sameEvent = response.eventId === event?.id || response.eventId === null;
    if (response.error === INCOGNITO_ERROR && sameEvent) return { kind: "discard", code: response.error };
    if (PERMANENT_ERRORS.has(response.error) && sameEvent) return { kind: "permanent", code: response.error };
    return { kind: "temporary", code: response.error };
  }
  return { kind: "temporary", code: "invalid_ack" };
}

/** A visit that carries observed navigation facts (an own `navigation` key, whatever its value). */
export const carriesNavigation = (event) => event?.type === "visit" && Object.hasOwn(event, "navigation");

/** The same visit without its navigation, for a host that cannot store one. */
function withoutNavigation(event) {
  const { navigation: _navigation, ...visit } = event;
  return visit;
}

const isUndeclared = (event) => (event.type === "visit" || event.type === "tab_snapshot") && event.incognito !== false;

const isRecording = (event) => event?.type === "visit" || event?.type === "tab_snapshot";

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

/**
 * `policy.current({ fresh })` resolves to `{ ok: true, version }`, and `policy.evaluate(urls)` to
 * `{ ok: true, version, decisions }`, a Map from URL to "allow" or "deny". Both resolve to
 * `{ ok: false, code }` when no decision can be made.
 */
/**
 * `capabilities()` resolves to `{ ok: true, navigation }` or `{ ok: false, code }` (host-capabilities.js).
 * Without it, no host is taken to store navigations.
 */
export function createEventQueue({ storage, send, policy, capabilities = async () => ({ ok: true, navigation: false }), now = () => new Date(), timeoutMs = SEND_TIMEOUT_MS, onChange = () => {} }) {
  if (typeof policy?.current !== "function") throw new TypeError("createEventQueue needs a policy");
  let tail = Promise.resolve();
  let sweptVersion = null;
  // Ids removed because of the policy, so `locate` does not report them as stored.
  const discarded = new Set();
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

  /** Removes `event`; `lost` names a navigation count to raise in the same write. */
  function remove(event, lost = null) {
    return exclusive(async () => {
      const queue = await readList(QUEUE_KEY);
      const items = { [QUEUE_KEY]: queue.filter((item) => !sameEvent(item, event)) };
      if (lost) {
        const stored = await storage.get(STATS_KEY);
        const stats = stored?.[STATS_KEY] && typeof stored[STATS_KEY] === "object" ? stored[STATS_KEY] : {};
        items[STATS_KEY] = { ...stats, [lost]: (Number(stats[lost]) || 0) + 1, lastNavigationLossAt: now().toISOString() };
      }
      await storage.set(items);
    });
  }

  function noteDiscarded(event) {
    if (!hasId(event)) return;
    if (discarded.size >= MAX_PENDING) discarded.clear();
    discarded.add(event.id);
  }

  /**
   * Removes what `decisions` refuses from both lists, in one write. Keeps no copy or count. An event
   * enqueued after the host was asked is not in `decisions`: it was allowed itself, and stays.
   */
  function sweep(decisions) {
    const refused = (event) => isRecording(event) && (typeof event.url !== "string" || decisions.get(event.url) === "deny");
    return exclusive(async () => {
      const stored = await storage.get([QUEUE_KEY, DEAD_LETTER_KEY]);
      const list = (key) => (Array.isArray(stored?.[key]) ? stored[key] : []);
      const queue = list(QUEUE_KEY);
      const dead = list(DEAD_LETTER_KEY);
      const removedIds = new Set();
      const keep = (event) => {
        if (!refused(event)) return true;
        if (hasId(event)) removedIds.add(event.id);
        return false;
      };
      const keptQueue = queue.filter((event) => {
        if (keep(event)) return true;
        noteDiscarded(event);
        return false;
      });
      const keptDead = dead.filter((entry) => keep(entry?.event));
      // A visit that stays may name a refused one as its source: that id goes too (audit F-4). Only the
      // reference changes; the event keeps its id and place, and the host still judges any source.
      let unlinked = 0;
      const unlink = (event) => {
        if (!carriesNavigation(event) || !removedIds.has(event.navigation?.sourceVisitId)) return event;
        unlinked += 1;
        return { ...event, navigation: { ...event.navigation, sourceVisitId: null } };
      };
      const finalQueue = keptQueue.map(unlink);
      const finalDead = keptDead.map((entry) => (entry?.event ? { ...entry, event: unlink(entry.event) } : entry));
      if (keptQueue.length === queue.length && keptDead.length === dead.length && unlinked === 0) return;
      await storage.set({ [QUEUE_KEY]: finalQueue, [DEAD_LETTER_KEY]: finalDead });
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
    // The first check of each drain asks the host; a long drain re-asks when the answer is old.
    let fresh = true;
    // Whether this host stores navigations: asked once per drain, and again after a doubtful ACK.
    let navigationSupport = null;
    for (;;) {
      const stored = await storage.get([QUEUE_KEY, DEAD_LETTER_KEY]);
      const pendingCount = Array.isArray(stored?.[QUEUE_KEY]) ? stored[QUEUE_KEY].length : 0;
      const deadCount = Array.isArray(stored?.[DEAD_LETTER_KEY]) ? stored[DEAD_LETTER_KEY].length : 0;
      if (pendingCount === 0 && (deadCount === 0 || !fresh)) return null;
      const verdict = await policy.current({ fresh });
      fresh = false;
      if (verdict?.ok !== true) return pendingCount > 0 ? verdict?.code || "policy_unavailable" : null;
      if (verdict.version !== sweptVersion) {
        const events = [...(Array.isArray(stored?.[QUEUE_KEY]) ? stored[QUEUE_KEY] : []), ...(Array.isArray(stored?.[DEAD_LETTER_KEY]) ? stored[DEAD_LETTER_KEY].map((entry) => entry?.event) : [])];
        const judged = await policy.evaluate(events.filter(isRecording).map((event) => event.url));
        if (judged?.ok !== true) return pendingCount > 0 ? judged?.code || "policy_unavailable" : null;
        try {
          await sweep(judged.decisions);
        } catch {
          return "storage_write_failed";
        }
        sweptVersion = judged.version;
      }
      const [head] = await readList(QUEUE_KEY);
      if (head === undefined) return null;
      let sent = head;
      if (carriesNavigation(head) && !isUndeclared(head)) {
        if (navigationSupport === null) {
          const answer = await capabilities();
          if (answer?.ok !== true) return typeof answer?.code === "string" ? answer.code : "host_unavailable";
          navigationSupport = answer.navigation === true;
        }
        if (!navigationSupport) sent = withoutNavigation(head);
      }
      const outcome = await deliver(sent);
      if (outcome.code === "dropped_by_policy") noteDiscarded(head);
      let lost = null;
      if (outcome.kind === "delivered") lost = sent !== head ? "navigationNotStored" : outcome.lost ?? null;
      if (outcome.lost === "navigationNotStored") navigationSupport = null;
      try {
        if (outcome.kind === "delivered") await remove(head, lost);
        else if (outcome.kind === "discard") await remove(head);
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
      navigationInvalid: Number(stored?.[STATS_KEY]?.navigationInvalid) || 0,
      navigationNotStored: Number(stored?.[STATS_KEY]?.navigationNotStored) || 0,
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
    let refused = 0;
    for (const id of ids) {
      if (pendingIds.has(id)) pending += 1;
      else if (deadIds.has(id)) deadLettered += 1;
      else if (discarded.delete(id)) refused += 1;
    }
    const where = { pending, deadLettered, stored: ids.length - pending - deadLettered - refused };
    return refused > 0 ? { ...where, refused } : where;
  }

  return { enqueue: (event) => enqueueAll([event]), enqueueAll, recordDropped, flush, status, locate };
}
