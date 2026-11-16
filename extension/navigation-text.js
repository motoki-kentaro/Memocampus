// How a navigation's source is shown (Issue #7). A source is one of three things, and they are never confused:
// a visit that is there, a visit the person deleted on purpose (`sourceCensored`, set only by that deletion), and a
// source that was never known, stored or allowed (a bare null). `[CENSORED]` is display text only: it is never
// stored in place of a URL or a title, and a null alone never produces it.
export const CENSORED_LABEL = "[CENSORED]";
export const UNKNOWN_SOURCE_LABEL = "不明";

/** `{ state: "present" | "censored" | "unknown", text }`; null for a row no host writes (a source that is also censored). */
export function navigationSource({ sourceVisitId = null, sourceCensored = false, describe = (id) => id } = {}) {
  if (typeof sourceVisitId === "string" && sourceVisitId !== "") {
    if (sourceCensored === true) return null;
    return { state: "present", text: describe(sourceVisitId) };
  }
  if (sourceCensored === true) return { state: "censored", text: CENSORED_LABEL };
  return { state: "unknown", text: UNKNOWN_SOURCE_LABEL };
}
