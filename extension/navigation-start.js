// First-run admission evidence, separate from the navigation/source tracker. Pending URLs/times in
// worker's memory: no persistence or logging. A commit/completion is never evidence of a start.
// BeforeNavigate has no documentId. Bind the pending start to the document only at Committed.
// Overlapping/repeated provisional starts keep the earliest time (unknown stays unknown), so a
// newer attempt/redirect cannot relabel an older request as starting after consent.
export const MAX_STARTED_DOCUMENTS = 4;
export const MAX_PENDING_STARTS = 16;

export function createNavigationStarts({ now = () => Date.now() } = {}) {
  const tabs = new Map();
  const mainFrame = (details) => details?.frameId === 0 && Number.isInteger(details.tabId) && details.tabId >= 0;
  const documentOf = (details) => typeof details.documentId === "string" && details.documentId !== "" ? details.documentId : null;
  function stateOf(tabId) {
    if (!tabs.has(tabId)) tabs.set(tabId, { pending: [], overflow: false, documents: new Map() });
    return tabs.get(tabId);
  }
  function takeStart(state, details) {
    if (state.overflow) return null;
    const redirected = details.transitionQualifiers?.includes("server_redirect") === true;
    const candidates = state.pending.map((start, index) => ({ ...start, index }))
      .filter((start) => redirected || (typeof details.url === "string" && start.url === details.url));
    if (candidates.length === 0) return null;
    const at = candidates.some((start) => start.at === null) ? null : Math.min(...candidates.map((start) => start.at));
    // A provisional sequence has no id. Consume only one attempt, retaining the oldest lower
    // bound until every matching overlapping attempt has committed/failed. Consuming the oldest
    // would let an older commit borrow a newer start after another navigation committed first.
    const known = candidates.filter((start) => start.at !== null);
    const newest = known.length ? known.reduce((a, b) => a.at > b.at ? a : b) : candidates.at(-1);
    state.pending.splice(newest.index, 1);
    return at;
  }
  return {
    before(details) {
      if (!mainFrame(details)) return;
      const at = Number.isFinite(details.timeStamp) && details.timeStamp > 0 ? Math.min(now(), Math.floor(details.timeStamp)) : null;
      const state = stateOf(details.tabId);
      if (state.pending.length >= MAX_PENDING_STARTS) {
        state.pending = [];
        state.overflow = true;
      }
      if (!state.overflow) state.pending.push({ at, url: details.url });
    },
    committed(details) {
      if (!mainFrame(details)) return;
      const state = stateOf(details.tabId);
      const document = documentOf(details);
      const at = takeStart(state, details);
      if (document !== null) {
        state.documents.delete(document);
        state.documents.set(document, at);
        if (state.documents.size > MAX_STARTED_DOCUMENTS) state.documents.delete(state.documents.keys().next().value);
      }
    },
    completed(details) {
      if (!mainFrame(details)) return null;
      const state = tabs.get(details.tabId);
      const document = documentOf(details);
      if (!state || document === null) return null;
      const at = state.documents.get(document) ?? null;
      state.documents.delete(document);
      return at;
    },
    failed(details) {
      if (!mainFrame(details)) return;
      const state = stateOf(details.tabId);
      // Errors without a document id belong to provisional loads. A document error must not
      // consume the start of a different navigation which is already pending in the same tab.
      if (documentOf(details) === null) takeStart(state, details);
      state.documents.delete(documentOf(details));
    },
    removed(tabId) { tabs.delete(tabId); },
  };
}
