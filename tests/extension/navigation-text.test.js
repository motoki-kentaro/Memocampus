import { test } from "node:test";
import assert from "node:assert/strict";
import { CENSORED_LABEL, navigationSource } from "../../extension/navigation-text.js";
import { t } from "../../extension/i18n.js";

const UNKNOWN_SOURCE_LABEL = t("navigationSourceUnknown");

test("a deleted source is [CENSORED], an absent one is unknown, and a null alone is never censored", () => {
  assert.equal(CENSORED_LABEL, "[CENSORED]");
  assert.deepEqual(navigationSource({ sourceVisitId: null, sourceCensored: true }), { state: "censored", text: "[CENSORED]" });
  assert.deepEqual(navigationSource({ sourceVisitId: null, sourceCensored: false }), { state: "unknown", text: UNKNOWN_SOURCE_LABEL });
  assert.deepEqual(navigationSource({}), { state: "unknown", text: UNKNOWN_SOURCE_LABEL }, "nothing said is unknown, not censored");
  assert.deepEqual(navigationSource({ sourceVisitId: "v1", sourceCensored: false, describe: (id) => `visit ${id}` }), { state: "present", text: "visit v1" });
  assert.equal(navigationSource({ sourceVisitId: "v1", sourceCensored: true }), null, "a row no host writes shows nothing");
  for (const odd of [undefined, 0, 1, "1", "true"]) assert.notEqual(navigationSource({ sourceVisitId: null, sourceCensored: odd }).state, "censored", String(odd));
});
