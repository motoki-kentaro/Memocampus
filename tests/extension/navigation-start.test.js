import { test } from "node:test";
import assert from "node:assert/strict";
import { createNavigationStarts, MAX_STARTED_DOCUMENTS, MAX_PENDING_STARTS } from "../../extension/navigation-start.js";

const event = (timeStamp, documentId, extra = {}) => ({ tabId: 1, frameId: 0, url: "https://nav.test/", timeStamp, ...(documentId ? { documentId } : {}), ...extra });
const make = () => createNavigationStarts({ now: () => 10_000 });

test("a request beginning before consent retains its start when both commit and completion happen afterwards", () => {
  const starts = make();
  starts.before(event(100));
  starts.committed(event(300, "delayed"));
  assert.equal(starts.completed(event(400, "delayed")), 100);
  starts.before(event(500));
  starts.committed(event(600, "control"));
  assert.equal(starts.completed(event(700, "control")), 500);
});

test("commit, completion, and a worker restart cannot substitute for a missing navigation start", () => {
  let starts = make();
  starts.committed(event(300, "unknown"));
  assert.equal(starts.completed(event(400, "unknown")), null);
  starts.before(event(100));
  starts = make();
  starts.committed(event(300, "restart-before-commit"));
  assert.equal(starts.completed(event(400, "restart-before-commit")), null);
  starts.before(event(500));
  starts.committed(event(600, "restart-after-commit"));
  starts = make();
  assert.equal(starts.completed(event(700, "restart-after-commit")), null);
});

test("start evidence is document-bound, consumed once, and never borrowed from a later request", () => {
  const starts = make();
  starts.before(event(100));
  starts.committed(event(200, "older"));
  starts.before(event(500));
  assert.equal(starts.completed(event(600, "unmatched")), null);
  starts.committed(event(700, "newer"));
  assert.equal(starts.completed(event(800, "older")), 100);
  assert.equal(starts.completed(event(900, "newer")), 500);
  assert.equal(starts.completed(event(901, "newer")), null);
});

test("overlapping requests and provisional redirects keep the earliest start rather than the newest attempt", () => {
  const starts = make();
  starts.before(event(100, null, { url: "https://first.test/" }));
  starts.before(event(500, null, { url: "https://redirect.test/" }));
  starts.committed(event(600, "redirected", { url: "https://redirect.test/", transitionQualifiers: ["server_redirect"] }));
  assert.equal(starts.completed(event(700, "redirected")), 100);
  starts.committed(event(800, "other-overlap", { url: "https://first.test/" }));
  assert.equal(starts.completed(event(900, "other-overlap")), 100);
});

test("a commit cannot consume all overlapping starts and relabel a surviving old request", () => {
  const starts = make();
  starts.before(event(100));
  starts.before(event(150));
  starts.committed(event(300, "first"));
  starts.before(event(500));
  starts.committed(event(600, "surviving-old"));
  assert.equal(starts.completed(event(700, "surviving-old")), 100);
  starts.committed(event(800, "new"));
  assert.equal(starts.completed(event(900, "new")), 100);
});

test("an unmatched commit after a restart cannot borrow another URL's observed start", () => {
  const starts = make();
  starts.before(event(500, null, { url: "https://control.test/" }));
  starts.committed(event(600, "missing", { url: "https://old.test/" }));
  assert.equal(starts.completed(event(700, "missing")), null);
  starts.committed(event(800, "control", { url: "https://control.test/" }));
  assert.equal(starts.completed(event(900, "control")), 500);
});

test("invalid times and overlapping errors never let an old request borrow a newer start", () => {
  for (const invalid of [null, undefined, Number.NaN, Infinity, 0, -1, "100"]) {
    const starts = make();
    starts.before(event(invalid));
    starts.before(event(500));
    starts.committed(event(600, "invalid"));
    assert.equal(starts.completed(event(700, "invalid")), null);
    const reversed = make();
    reversed.before(event(500));
    reversed.before(event(invalid));
    reversed.committed(event(600, "first"));
    reversed.committed(event(700, "second"));
    assert.equal(reversed.completed(event(800, "first")), null);
    assert.equal(reversed.completed(event(900, "second")), null);
  }
  const starts = make();
  starts.before(event(100));
  starts.before(event(500));
  starts.failed(event(550));
  starts.committed(event(600, "ambiguous"));
  assert.equal(starts.completed(event(700, "ambiguous")), 100);
  starts.before(event(800));
  starts.committed(event(900, "fresh"));
  assert.equal(starts.completed(event(1000, "fresh")), 800);
});

test("overflowed provisional evidence remains unknown rather than retaining only newer starts", () => {
  const starts = make();
  for (let n = 0; n <= MAX_PENDING_STARTS; n++) starts.before(event(n + 100));
  starts.committed(event(600, "overflow"));
  assert.equal(starts.completed(event(700, "overflow")), null);
});

test("subframes, unknown documents and evicted evidence stay unknown; removed/replaced tabs cannot retain evidence", () => {
  const starts = make();
  starts.before(event(100, null, { frameId: 1 }));
  starts.committed(event(200, "main"));
  assert.equal(starts.completed(event(300, "main")), null);
  starts.before(event(400));
  starts.committed(event(500, "known"));
  assert.equal(starts.completed(event(600, "known", { frameId: 1 })), null);
  assert.equal(starts.completed(event(600, "known")), 400);
  starts.before(event(700));
  starts.committed(event(800));
  assert.equal(starts.completed(event(900)), null);
  for (let n = 0; n <= MAX_STARTED_DOCUMENTS; n++) {
    starts.before(event(1000 + n));
    starts.committed(event(1100 + n, `bounded-${n}`));
  }
  assert.equal(starts.completed(event(1200, "bounded-0")), null);
  starts.removed(1);
  assert.equal(starts.completed(event(1300, "bounded-1")), null);
});
