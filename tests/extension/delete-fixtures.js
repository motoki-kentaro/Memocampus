// A stand-in for the Native Host's reviewed deletion (Issue #7, `item_*` and `visit_history` of item_delete.rs),
// over one fake connectNative port. It keeps the Host's contract: a preview fixes the exact set (ids and rows),
// one review id confirms it once, only on the port that issued it, only for the same Vault token and
// selection generation, and only if the set is still what it was.
//
// State lives on the fake host (`createHost`): `visits[vault]` = [{ id, url, title, visitedAt }],
// `snapshots[vault]` = [url, ...] (one per tab snapshot), `images[vault]` / `tags[vault]` as the Image tab's,
// `vaults[vault]` = the URL list rows the search answers with. `generations[vault]` is the selection generation.
const tokenOf = (vault) => vault.charCodeAt(0).toString(16).padStart(2, "0").repeat(32);
const event = () => { const listeners = []; return { addListener: (fn) => listeners.push(fn), emit: (...args) => listeners.forEach((fn) => fn(...args)) }; };
const fold = (s) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());

export function deleteState(host) {
  host.visits ??= {};
  host.snapshots ??= {};
  host.generations ??= {};
  host.deleteRequests ??= [];
  host.deletePorts ??= [];
  host.deleteDelays ??= {};
  host.deleteDialogs ??= [];
  return host;
}

const generationOf = (host, vault) => host.generations[vault] ?? "generation-1";
const matchesQuery = (visit, query) => !query || fold(visit.url).includes(fold(query)) || fold(visit.title).includes(fold(query));
const inRange = (visit, from, to) => from === null || from === undefined || (visit.visitedAt >= from && visit.visitedAt < to);

function visitSet(host, vault, target) {
  const all = host.visits[vault] ?? [];
  if (target.scope === "visit") return all.filter((v) => v.id === target.id);
  if (target.scope === "visits") return all.filter((v) => matchesQuery(v, (target.query ?? "").trim()) && inRange(v, target.visitedFrom, target.visitedTo));
  return [];
}

function describe(host, vault, target) {
  if (target.scope === "visit" || target.scope === "visits") {
    const rows = visitSet(host, vault, target);
    const urls = new Set(rows.map((v) => v.url));
    const snapshotsKept = (host.snapshots[vault] ?? []).filter((u) => urls.has(u)).length;
    return { rows, counts: { kind: "visits", visits: rows.length, urls: urls.size, pageMetadata: 0, navigations: 0, navigationSourcesCleared: 0, snapshotsKept, ...(host.navigationCounts ?? {}) }, detail: target.scope === "visit" && rows[0] ? { ...rows[0] } : null, has: rows.length > 0 };
  }
  if (target.scope === "url") {
    const visits = (host.visits[vault] ?? []).filter((v) => v.url === target.url);
    const snaps = (host.snapshots[vault] ?? []).filter((u) => u === target.url);
    return { rows: [visits, snaps.length], counts: { kind: "url", urls: visits.length + snaps.length > 0 ? 1 : 0, visits: visits.length, tabSnapshots: snaps.length, pageMetadata: visits.length ? 1 : 0, navigations: 0, navigationSourcesCleared: 0, imageSavesKept: host.imageSavesKept ?? 0 }, detail: null, has: visits.length + snaps.length > 0 };
  }
  const images = host.images[vault] ?? [];
  let set;
  if (target.scope === "image") set = images.filter((i) => i.blobId === target.blobId && i.sha256 === target.sha256);
  else {
    const tag = target.tag === null || target.tag === undefined ? null : host.normalizeTag(target.tag);
    set = tag === null ? images : images.filter((i) => (host.tags[vault]?.[i.blobId] ?? []).includes(tag));
  }
  const ids = new Set(set.map((i) => i.blobId));
  const tags = host.tags[vault] ?? {};
  const names = new Set(set.flatMap((i) => tags[i.blobId] ?? []));
  const keep = Object.entries(tags).filter(([id]) => !ids.has(Number(id))).flatMap(([, n]) => n);
  return { rows: set.map((i) => ({ ...i, tags: tags[i.blobId] ?? [] })), counts: { kind: "images", images: set.length, bytes: set.reduce((a, i) => a + i.byteLength, 0), saves: set.reduce((a, i) => a + (i.sources?.length ?? i.saveCount), 0), thumbnails: set.filter((i) => i.hasThumbnail).length, tagLinks: set.reduce((a, i) => a + (tags[i.blobId] ?? []).length, 0), tagsRemoved: [...names].filter((n) => !keep.includes(n)).length }, detail: null, has: set.length > 0 };
}

function apply(host, vault, target) {
  if (target.scope === "visit" || target.scope === "visits") {
    const gone = new Set(visitSet(host, vault, target).map((v) => v.id));
    const touchedRows = visitSet(host, vault, target);
    host.visits[vault] = (host.visits[vault] ?? []).filter((v) => !gone.has(v.id));
    // The URL list the search answers with: a URL stays while a visit or a snapshot of it does.
    const touched = new Set(touchedRows.map((v) => v.url));
    host.vaults[vault] = (host.vaults[vault] ?? []).filter((page) => !touched.has(page.url) || (host.visits[vault] ?? []).some((v) => v.url === page.url) || (host.snapshots[vault] ?? []).includes(page.url));
  } else if (target.scope === "url") {
    host.visits[vault] = (host.visits[vault] ?? []).filter((v) => v.url !== target.url);
    host.snapshots[vault] = (host.snapshots[vault] ?? []).filter((u) => u !== target.url);
    host.vaults[vault] = (host.vaults[vault] ?? []).filter((page) => page.url !== target.url);
    host.deletedUrls.push(target.url);
  } else {
    const removed = describe(host, vault, target).rows.map((i) => i.blobId);
    host.images[vault] = (host.images[vault] ?? []).filter((i) => !removed.includes(i.blobId));
    for (const id of removed) { delete host.tags[vault]?.[id]; host.deletedImages.push(id); }
  }
}

export function createDeletePort(host) {
  deleteState(host);
  const context = { review: null, receipt: null, binding: null };
  let closed = false;
  const port = {
    onMessage: event(),
    onDisconnect: event(),
    disconnect() { if (closed) return; closed = true; port.onDisconnect.emit(); },
    postMessage(message) {
      host.deleteRequests.push(structuredClone(message));
      void (async () => {
        await new Promise((resolve) => setTimeout(resolve, host.deleteDelays[message.type] ?? 0));
        if (host.deleteDelayFn?.[message.type]) await host.deleteDelayFn[message.type](message);
        if (closed) return;
        const vault = host.active;
        const type = `${message.type}_result`;
        const send = (fields) => { if (!closed) port.onMessage.emit({ type, requestId: message.requestId, ...fields }); };
        if (!vault) return send({ ok: false, error: "vault_not_configured" });
        const binding = { vaultToken: tokenOf(vault), generation: generationOf(host, vault) };
        const envelope = { ok: true, ...binding };
        if (host.deleteFailure) { const error = host.deleteFailure; if (host.deleteFailureOnce !== false) host.deleteFailure = null; return send({ ok: false, error }); }
        if (context.binding && (context.binding.vaultToken !== binding.vaultToken || context.binding.generation !== binding.generation)) { context.review = null; context.receipt = null; }
        context.binding = binding;
        if (message.type !== "item_state" && (message.vaultToken !== binding.vaultToken || message.generation !== binding.generation)) return send({ ok: false, error: "vault_switched" });
        if (message.type === "item_state") return send(envelope);
        if (message.type === "visit_history") {
          const filter = { query: (message.query ?? "").trim(), from: message.visitedFrom, to: message.visitedTo };
          const mine = (host.visits[vault] ?? []).filter((v) => v.url === message.url);
          const matched = mine.filter((v) => matchesQuery(v, filter.query) && inRange(v, filter.from, filter.to)).sort((a, b) => (a.visitedAt < b.visitedAt ? 1 : a.visitedAt > b.visitedAt ? -1 : a.id < b.id ? 1 : -1));
          const after = message.after;
          const rest = after ? matched.filter((v) => v.visitedAt < after.visitedAt || (v.visitedAt === after.visitedAt && v.id < after.id)) : matched;
          const limit = message.limit ?? 20;
          const page = rest.slice(0, limit);
          return send({ ...envelope, items: page.map(({ id, title, visitedAt }) => ({ id, title, visitedAt })), next: rest.length > limit ? { visitedAt: page.at(-1).visitedAt, id: page.at(-1).id } : null, counts: { matched: matched.length, total: mine.length, snapshots: (host.snapshots[vault] ?? []).filter((u) => u === message.url).length } });
        }
        if (message.type === "item_delete_preview") {
          context.review = null;
          const target = structuredClone(message.target);
          if (target.scope === "images" && target.tag !== null && target.tag !== undefined) target.tag = host.normalizeTag(target.tag);
          const d = describe(host, vault, target);
          if (!d.has) return send({ ok: false, error: "item_not_found" });
          const reviewId = crypto.randomUUID();
          context.review = { reviewId, target, fingerprint: JSON.stringify([d.rows, d.counts]), counts: d.counts, binding };
          return send({ ...envelope, reviewId, target, counts: d.counts, detail: d.detail, expiresInSeconds: 120 });
        }
        if (message.type === "item_delete") {
          const review = context.review;
          if (!review || review.reviewId !== message.reviewId) return send({ ok: false, error: "invalid_review" });
          context.review = null;
          const d = describe(host, vault, review.target);
          if (JSON.stringify([d.rows, d.counts]) !== review.fingerprint) return send({ ok: false, error: "item_targets_changed" });
          apply(host, vault, review.target);
          host.deleteCommits = (host.deleteCommits ?? 0) + 1;
          context.receipt = { operationId: review.reviewId, deleted: review.counts };
          // The review's reproduction: the COMMIT happened, the answer never arrives, the port ends.
          if (host.commitThenDisconnect) { host.commitThenDisconnect = false; port.disconnect(); return; }
          if (host.dropDeleteReply) { host.dropDeleteReply = false; return; }
          if (host.garbleDeleteReply) { host.garbleDeleteReply = false; return port.onMessage.emit({ type: "something_else", requestId: message.requestId, ok: true }); }
          // `deleteReplyShape(good)`: a test's contract-breaking receipt (fields missing, mistyped or of another operation).
          const receipt = { ...envelope, operationId: review.reviewId, deleted: review.counts };
          return send(host.deleteReplyShape ? host.deleteReplyShape(receipt) : receipt);
        }
        if (message.type === "item_delete_status") {
          const known = context.receipt?.operationId === message.operationId;
          const status = { ...envelope, operationId: message.operationId, confirmed: known, ...(known ? { deleted: context.receipt.deleted } : {}) };
          return send(host.statusReplyShape ? host.statusReplyShape(status) : status);
        }
        return send({ ok: false, error: "unknown_type" });
      })();
    },
  };
  host.deletePorts.push(port);
  return port;
}

export const visitRow = (id, url, title = `visit ${id}`, visitedAt = `2026-10-${String(10 + (Number(String(id).replace(/\D/g, "")) % 18)).padStart(2, "0")}T00:00:00.000Z`) => ({ id: String(id), url, title, visitedAt });
