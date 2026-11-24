// How Chrome reads "$" in an extension's messages.json (Issue #77), shared by the chrome.i18n mock
// (i18n-mock.js) and the static catalog checks (i18n-catalog.js), so that both agree with Chrome.
//
// The two steps copy Chrome's own algorithms (extensions MessageBundle::ReplaceVariables at load time,
// base::ReplaceStringPlaceholders in getMessage) and were checked against real Chrome for Testing 158
// (tests/dogfood/i18n-chrome.mjs):
//   load time:  "$name$" (name of A-Z a-z 0-9 _ @, case-insensitive) becomes the placeholder's
//               content; an undefined name makes Chrome refuse to load the whole extension. Scanning
//               pairs every "$" with the next "$", so "$$name$$" still references "name".
//   getMessage: "$1"…"$9" take the substitutions (one digit only); a run of n >= 2 "$" becomes n - 1
//               "$" (so "$$" is a literal "$"); a "$" before anything else is dropped with that
//               character, and a final "$" is dropped. Substituted values are never read again.
// Strings in manifest.json (__MSG_key__) get only the load-time step: "$$" stays "$$" there.

const VARIABLE_NAME = /^[A-Za-z0-9_@]+$/;

/**
 * Chrome's load-time step (MessageBundle::ReplaceVariables): every "$name$" becomes its placeholder's
 * content. Returns the expanded text, the names referenced (lowercased), the ranges of `text` that came
 * from placeholder contents, and `error` (Chrome's own wording) when Chrome would refuse to load.
 */
export function expandPlaceholders(message, placeholders) {
  const contents = placeholders instanceof Map ? placeholders
    : new Map(Object.entries(placeholders ?? {}).map(([name, value]) => [name.toLowerCase(), value?.content]));
  let text = message;
  const references = [];
  const inserted = [];
  let begin = 0;
  for (;;) {
    begin = text.indexOf("$", begin);
    if (begin === -1) break;
    begin += 1;
    if (begin >= text.length) break;
    const end = text.indexOf("$", begin);
    if (end === -1) break;
    const name = text.slice(begin, end);
    if (!VARIABLE_NAME.test(name)) continue;
    references.push(name.toLowerCase());
    const content = contents.get(name.toLowerCase());
    if (typeof content !== "string") return { text, references, inserted, error: `Variable $${name}$ used but not defined.` };
    text = text.slice(0, begin - 1) + content + text.slice(end + 1);
    inserted.push([begin - 1, begin - 1 + content.length]);
    begin = begin - 1 + content.length;
  }
  return { text, references, inserted, error: null };
}

/**
 * Chrome's getMessage step (base::ReplaceStringPlaceholders), one pass over the format only. `tokens`
 * lists what each "$" turned into, for the static checks: literal runs, substitutions and dropped text.
 */
export function substitute(format, values = []) {
  let out = "";
  const tokens = [];
  for (let i = 0; i < format.length; i += 1) {
    if (format[i] !== "$") { out += format[i]; continue; }
    const at = i;
    if (i + 1 === format.length) { tokens.push({ kind: "dropped", at, text: "$" }); break; }
    i += 1;
    if (format[i] === "$") {
      let run = 1;
      while (i < format.length && format[i] === "$") { out += "$"; run += 1; i += 1; }
      i -= 1;
      tokens.push({ kind: "literal", at, run });
    } else if (format[i] >= "1" && format[i] <= "9") {
      const index = Number(format[i]) - 1;
      out += values[index] ?? "";
      tokens.push({ kind: "substitution", at, index });
    } else {
      tokens.push({ kind: "dropped", at, text: `$${format[i]}` });
    }
  }
  return { text: out, tokens };
}

/** The number of substitutions a format takes: its highest $n. */
export function substitutionCount(format) {
  return Math.max(0, ...substitute(format).tokens.filter((token) => token.kind === "substitution").map((token) => token.index + 1));
}
