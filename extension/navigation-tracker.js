// Observed navigation facts (Issue #41): how Chrome reported arriving at a visit's document. The
// tracker only correlates webNavigation events; it never reads a page, never decides privacy and sends
// nothing. The service worker asks it for the `navigation` of a document visit it is about to record.
//
// Meaning (PO decisions H2–H8): `sourceVisitId` is the recorded visit of the document shown before, in
// the same tab or, for a new tab, in its opener's main frame. It is never proof of a click. When the
// tracker cannot be sure, the source is null: it never guesses from URLs or timing alone.
//
// Scope (PO decision after the audit): documents only. A URL change inside a document (pushState,
// replaceState, fragments) is never recorded: the application keeps its own state. A history update is
// used only to forget: the tab no longer shows a known recorded visit, the document's pending commit is
// dropped (its facts no longer describe what the tab shows), and the document is marked so that a
// title or page read from it is not trusted (`pristine`).
//
// State, per tab, in this worker's memory only. Nothing is persisted: a stopped worker starts with no
// state, so the first navigations after it have no source and their commits are not paired.
//   lastDocumentId  the last main-frame document committed in the tab
//   head            { token, visitId }: what the tab shows now; `visitId` once its visit is queued
//                   (`recorded`). A commit or a history update replaces or clears it.
//   pending         commits not yet completed, by documentId (observed facts, fixed source, URL)
//   branch          an onCreatedNavigationTarget waiting for the new tab's first http(s) commit
//                   (to an http URL, or to about:blank that the opener then sends elsewhere)
//   touched         recent documents that had a history update
// Incognito tabs (and tabs whose kind cannot be read) have no state at all.
//
// Serialization: every handler runs its state step on a per-tab chain (only `isNormalTab` is awaited
// in it). A branch reads its source tab and then updates the new tab in two separate steps, so no step
// waits for another tab's chain while holding one.

/** A branch not matched to the new tab's first commit within this time (Chrome timestamps) is dropped. */
export const BRANCH_TTL_MS = 30_000;
/** Commits kept per tab while waiting for onCompleted; the oldest is dropped beyond this. */
export const MAX_PENDING_COMMITS = 4;
/** Documents remembered per tab as having had a history update. */
export const MAX_TOUCHED_DOCUMENTS = 16;
const MAX_ID = 128;

/** Chrome's TransitionType values, as the host accepts them. Anything else is sent as "unknown". */
export const TRANSITION_TYPES = new Set([
  "link",
  "typed",
  "auto_bookmark",
  "auto_subframe",
  "manual_subframe",
  "generated",
  "start_page",
  "form_submit",
  "reload",
  "keyword",
  "keyword_generated",
]);

/**
 * Transitions that do not continue from the document shown before (the address bar, a bookmark, the
 * omnibox, the start page) and an unclassified one: their source is null, except on back or forward
 * (`sourceAllowed`), and they never take a new-tab branch.
 */
export const SOURCELESS_TRANSITIONS = new Set(["typed", "auto_bookmark", "generated", "start_page", "keyword", "keyword_generated", "unknown"]);

const QUALIFIERS = [
  ["client_redirect", "clientRedirect"],
  ["server_redirect", "serverRedirect"],
  ["forward_back", "forwardBack"],
  ["from_address_bar", "fromAddressBar"],
];

const TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;

export function transitionOf(raw) {
  return TRANSITION_TYPES.has(raw) ? raw : "unknown";
}

/** Chrome's qualifiers as the four flags; unknown qualifiers are ignored, nothing is inferred. */
export function flagsOf(qualifiers) {
  const list = Array.isArray(qualifiers) ? qualifiers : [];
  return Object.fromEntries(QUALIFIERS.map(([chrome, key]) => [key, list.includes(chrome)]));
}

/** A Chrome `timeStamp` (ms since the epoch) as the host's 24-character UTC form, or null. */
export function occurredAtOf(timeStamp) {
  if (typeof timeStamp !== "number" || !Number.isFinite(timeStamp) || timeStamp <= 0) return null;
  const date = new Date(Math.floor(timeStamp));
  if (Number.isNaN(date.getTime())) return null;
  const text = date.toISOString();
  return TIMESTAMP.test(text) ? text : null;
}

/** A navigation that continues from the page shown before (not the address bar, a bookmark, ...). */
const continuesFromPage = (transitionType, flags) => !SOURCELESS_TRANSITIONS.has(transitionType) && !flags.fromAddressBar;

/**
 * Whether a navigation may name the visit shown before. Back and forward come first: Chrome then
 * reports the history entry's original transition and qualifiers (`typed`, `from_address_bar`, ...),
 * which say how that entry was first reached, not what happened now. The page shown before is still
 * the one left, so `forward_back` alone decides (Slice 3, I-2). The reported values are kept as they are.
 */
export const sourceAllowed = (transitionType, flags) => flags.forwardBack === true || continuesFromPage(transitionType, flags);

/** The `navigation` of a visit, exactly the nine keys of the host's contract. */
export function buildNavigation({ kind, transitionType, flags, sourceVisitId, newTab, occurredAt }) {
  return {
    sourceVisitId: typeof sourceVisitId === "string" ? sourceVisitId : null,
    kind,
    transitionType,
    clientRedirect: flags.clientRedirect === true,
    serverRedirect: flags.serverRedirect === true,
    forwardBack: flags.forwardBack === true,
    fromAddressBar: flags.fromAddressBar === true,
    newTab: newTab === true,
    occurredAt,
  };
}

const isHttp = (url) => {
  try {
    const { protocol } = new URL(url);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
};
const isAboutBlank = (url) => typeof url === "string" && url.startsWith("about:blank");
const isId = (value) => typeof value === "string" && value.length > 0 && value.length <= MAX_ID && /^[\x21-\x7e]+$/.test(value);
const isTabId = (value) => Number.isInteger(value) && value >= 0;

const freshState = () => ({ lastDocumentId: null, httpCommits: 0, head: null, pending: [], branch: null, touched: [] });

/** `isNormalTab(tabId)`: true only for a tab known not to be incognito. */
export function createNavigationTracker({ isNormalTab, randomId = () => crypto.randomUUID(), branchTtlMs = BRANCH_TTL_MS }) {
  const tabs = new Map();
  const chains = new Map();

  /** Runs `task` after every earlier step of the same tab. Steps of different tabs do not wait. */
  function withTab(tabId, task) {
    const run = (chains.get(tabId) ?? Promise.resolve()).then(task);
    const settled = run.then(
      () => {},
      () => {},
    );
    chains.set(tabId, settled);
    settled.then(() => {
      if (chains.get(tabId) === settled) chains.delete(tabId);
    });
    return run;
  }

  async function normal(tabId) {
    try {
      return (await isNormalTab(tabId)) === true;
    } catch {
      return false;
    }
  }

  // A branch to an http URL must arrive at that URL (or a server redirect from it). A branch to
  // about:blank (`targetUrl: null`) is sent on by the opener's script, so its URL cannot match; it is
  // matched only as the first http(s) commit of that very tab, about:blank alone in between.
  const branchFits = (branch, { url, committedAt, transitionType, flags }) =>
    Math.abs(committedAt - branch.timeStamp) <= branchTtlMs &&
    continuesFromPage(transitionType, flags) &&
    (branch.targetUrl === null || branch.targetUrl === url || flags.serverRedirect);

  /** The source a new navigation of this tab may name: the recorded visit the tab shows. */
  const shownVisit = (state) => state?.head?.visitId ?? null;

  /** webNavigation.onCommitted */
  function committed(details) {
    const { tabId, frameId, documentId, url } = details ?? {};
    if (frameId !== 0 || !isTabId(tabId) || details.documentLifecycle === "prerender") return Promise.resolve();
    return withTab(tabId, async () => {
      if (!(await normal(tabId))) {
        tabs.delete(tabId);
        return;
      }
      const state = tabs.get(tabId) ?? freshState();
      if (!isId(documentId)) {
        tabs.set(tabId, { ...freshState(), httpCommits: state.httpCommits + 1 });
        return;
      }
      // The same commit reported twice changes nothing.
      if (state.lastDocumentId === documentId && state.pending.some((entry) => entry.documentId === documentId)) return;
      const http = isHttp(url);
      const transitionType = transitionOf(details.transitionType);
      const flags = flagsOf(details.transitionQualifiers);
      const occurredAt = occurredAtOf(details.timeStamp);
      const first = http && state.httpCommits === 0;
      let sourceVisitId = sourceAllowed(transitionType, flags) ? shownVisit(state) : null;
      let newTab = false;
      let branch = state.branch;
      if (branch && http) {
        if (first && branchFits(branch, { url, committedAt: details.timeStamp, transitionType, flags })) {
          sourceVisitId = branch.sourceVisitId;
          newTab = true;
        }
        branch = null;
      } else if (branch && !isAboutBlank(url)) {
        branch = null;
      }
      const token = randomId();
      const pending = state.pending.filter((entry) => entry.documentId !== documentId);
      if (http && occurredAt) pending.push({ token, documentId, url, first, committedAt: details.timeStamp, observed: { transitionType, flags, sourceVisitId, newTab, occurredAt } });
      tabs.set(tabId, {
        lastDocumentId: documentId,
        httpCommits: state.httpCommits + (http ? 1 : 0),
        head: { token, visitId: null },
        pending: pending.slice(-MAX_PENDING_COMMITS),
        branch,
        touched: state.touched,
      });
    });
  }

  /** webNavigation.onCreatedNavigationTarget. Applies to the new tab's first http(s) commit only. */
  async function createdNavigationTarget(details) {
    const { sourceTabId, sourceFrameId, tabId, url, timeStamp } = details ?? {};
    const http = isHttp(url);
    if (!isTabId(tabId) || !(http || isAboutBlank(url)) || typeof timeStamp !== "number" || !Number.isFinite(timeStamp)) return;
    // An opener in a subframe (H5), or one this tracker does not know, gives no source.
    let sourceVisitId = null;
    if (sourceFrameId === 0 && isTabId(sourceTabId) && sourceTabId !== tabId) {
      sourceVisitId = await withTab(sourceTabId, async () => shownVisit(tabs.get(sourceTabId)));
    }
    await withTab(tabId, async () => {
      if (!(await normal(tabId))) {
        tabs.delete(tabId);
        return;
      }
      const state = tabs.get(tabId) ?? freshState();
      const branch = { sourceVisitId, targetUrl: http ? url : null, timeStamp };
      if (state.httpCommits === 0) {
        tabs.set(tabId, { ...state, branch });
        return;
      }
      // The first commit came first: it takes the branch only while it is still waiting to complete.
      const first = state.httpCommits === 1 ? state.pending.find((entry) => entry.first) : undefined;
      if (!first || !branchFits(branch, { url: first.url, committedAt: first.committedAt, transitionType: first.observed.transitionType, flags: first.observed.flags })) return;
      const pending = state.pending.map((entry) => (entry === first ? { ...entry, observed: { ...entry.observed, sourceVisitId, newTab: true } } : entry));
      tabs.set(tabId, { ...state, pending });
    });
  }

  /**
   * webNavigation.onCompleted: `{ token, navigation }` for the visit of this document, or null when its
   * commit was not observed, was dropped by a history update, or was at another URL (the visit is then
   * recorded without a navigation).
   */
  function completed(details) {
    const { tabId, frameId, documentId, url } = details ?? {};
    if (frameId !== 0 || !isTabId(tabId) || !isId(documentId) || details.documentLifecycle === "prerender") return Promise.resolve(null);
    return withTab(tabId, async () => {
      const state = tabs.get(tabId);
      const entry = state?.pending.find((candidate) => candidate.documentId === documentId);
      if (!entry) return null;
      tabs.set(tabId, { ...state, pending: state.pending.filter((candidate) => candidate !== entry) });
      if (entry.url !== url) return null;
      return { token: entry.token, navigation: buildNavigation({ kind: "document", ...entry.observed }) };
    });
  }

  /**
   * webNavigation.onHistoryStateUpdated (pushState / replaceState): never a visit. The tab no longer
   * shows its recorded visit (no source until the next recorded document), the document's pending
   * commit is dropped, and the document is no longer pristine.
   */
  function historyStateUpdated(details) {
    const { tabId, frameId, documentId } = details ?? {};
    if (frameId !== 0 || !isTabId(tabId)) return Promise.resolve();
    return withTab(tabId, async () => {
      const state = tabs.get(tabId) ?? ((await normal(tabId)) ? freshState() : null);
      if (!state) return;
      const touched = isId(documentId) ? [...state.touched.filter((id) => id !== documentId), documentId].slice(-MAX_TOUCHED_DOCUMENTS) : state.touched;
      tabs.set(tabId, { ...state, head: null, pending: state.pending.filter((entry) => entry.documentId !== documentId), touched });
    });
  }

  /**
   * Whether a title or page read from this document may be kept: no history update was seen in it.
   * Asked after reading, so an update that happened meanwhile counts too.
   */
  function pristine(tabId, documentId) {
    if (!isTabId(tabId) || !isId(documentId)) return Promise.resolve(false);
    return withTab(tabId, async () => !(tabs.get(tabId)?.touched.includes(documentId) ?? false));
  }

  /** The visit of `token` is queued as `visitId`: it may now be a source, if it is still shown. */
  function recorded(tabId, token, visitId) {
    if (!isTabId(tabId) || !isId(token) || !isId(visitId)) return Promise.resolve();
    return withTab(tabId, async () => {
      const state = tabs.get(tabId);
      if (state?.head?.token !== token) return;
      tabs.set(tabId, { ...state, head: { token, visitId } });
    });
  }

  /** tabs.onRemoved: the tab's state and any branch waiting in it are gone. */
  const tabRemoved = (tabId) => (isTabId(tabId) ? withTab(tabId, async () => void tabs.delete(tabId)) : Promise.resolve());

  /** tabs.onReplaced: neither tab's history continues into the other. */
  const tabReplaced = (addedTabId, removedTabId) => Promise.all([tabRemoved(addedTabId), tabRemoved(removedTabId)]).then(() => {});

  return { committed, createdNavigationTarget, completed, historyStateUpdated, pristine, recorded, tabRemoved, tabReplaced };
}
