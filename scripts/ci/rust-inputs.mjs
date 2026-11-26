// Issue #93 S3-2: finds the files outside a Rust crate that the crate reads, so that tests can check that changing any
// of them makes CI run the Rust job (scripts/ci/classify-changes.mjs).
//
// This is a pattern scan over a small Rust lexer (comments removed, string literals decoded), not a compiler. It finds:
//   - include_str! / include_bytes! / include! with a string literal, or concat!(env!("CARGO_MANIFEST_DIR"), literals);
//   - #[path = "..."] (also inside cfg_attr), resolved by Rust's rules for inline modules and mod-rs files;
//   - env!/option_env!/env::var("CARGO_MANIFEST_DIR") inside Path::new / PathBuf::from followed by literal .join() calls,
//     or inside concat! with literals;
//   - in build.rs: cargo:rerun-if-changed= values and fs reads, with a literal or a `const NAME: &str` argument;
//   - any string literal that names an existing path outside the crate (relative to the file, the crate or the
//     repository root), and any string literal with a `..` segment;
//   - `path`, `build` and `workspace` in Cargo.toml as basic ("...") or literal ('...') strings, under bare, quoted or
//     dotted keys, and every path-like string in .cargo/config.toml (multi-line strings and other values: unresolved).
// Whatever it cannot resolve is reported as `unresolved`, never as safe: an include! whose argument is not a literal,
// CARGO_MANIFEST_DIR used any other way, a .join() with a non-literal argument, a `..` literal that names no existing
// file, a build.rs read of a non-constant path. Tests compare that list with a reviewed one.
//
// It does not see paths assembled at run time from values it cannot read (format!, variables, environment variables
// other than CARGO_MANIFEST_DIR, current_dir()), files read by other programs a test starts, macro_rules! that wrap
// include_str!($x), or what dependency crates and proc macros read. See tests/ci/rust-inputs.test.js for examples.
import { existsSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";

const posix = path.posix;

/** Removes comments and replaces each string literal with §n§ (keeping line breaks); returns the code and the strings. */
export function lexRust(src) {
  const strings = [];
  const parts = [];
  let out = "";
  let i = 0;
  let line = 1;
  let lineAtIndex = 0;
  const flush = () => { if (out) { parts.push(out); out = ""; } };
  const lineOf = (index) => {
    for (; lineAtIndex < index; lineAtIndex++) if (src.charCodeAt(lineAtIndex) === 10) line++;
    return line;
  };
  const ident = (ch) => ch !== undefined && /[A-Za-z0-9_]/.test(ch);
  const push = (value, start) => {
    strings.push({ value, line: lineOf(start) });
    let breaks = 0;
    for (let k = start; k < i; k++) if (src.charCodeAt(k) === 10) breaks++;
    flush();
    parts.push(`§${strings.length - 1}§` + "\n".repeat(breaks));
  };
  while (i < src.length) {
    const c = src[i];
    if (c === "/" && src[i + 1] === "/") {
      const end = src.indexOf("\n", i);
      i = end < 0 ? src.length : end;
      continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      let depth = 0;
      do {
        if (src[i] === "/" && src[i + 1] === "*") { depth++; i += 2; continue; }
        if (src[i] === "*" && src[i + 1] === "/") { depth--; i += 2; continue; }
        if (src[i] === "\n") out += "\n";
        i++;
      } while (depth > 0 && i < src.length);
      continue;
    }
    if (c !== "b" && c !== "c" && c !== "r" && c !== '"' && c !== "'") {
      // Copy a run of ordinary characters at once.
      let j = i + 1;
      while (j < src.length && !'/bcr"\''.includes(src[j])) j++;
      out += src.slice(i, j);
      if (out.length > 65536) flush();
      i = j;
      continue;
    }
    const raw = c === '"' || c === "'" ? null : /^(?:b|c)?r(#*)"/.exec(src.slice(i, i + 300));
    if (raw && !ident(src[i - 1])) {
      const start = i;
      const close = '"' + raw[1];
      const end = src.indexOf(close, i + raw[0].length);
      if (end < 0) throw new Error("unterminated raw string");
      const value = src.slice(i + raw[0].length, end);
      i = end + close.length;
      push(value, start);
      continue;
    }
    const plain = /^(?:b|c)?"/.exec(src.slice(i, i + 2));
    if (plain && (plain[0] === '"' || !ident(src[i - 1]))) {
      const start = i;
      i += plain[0].length;
      let value = "";
      while (i < src.length && src[i] !== '"') {
        if (src[i] === "\\") {
          const e = src[i + 1];
          const simple = { n: "\n", r: "\r", t: "\t", "\\": "\\", "0": "\0", "'": "'", '"': '"' };
          if (e in simple) { value += simple[e]; i += 2; }
          else if (e === "x") { value += String.fromCharCode(parseInt(src.slice(i + 2, i + 4), 16)); i += 4; }
          else if (e === "u") { const m = /^u\{([0-9a-fA-F_]{1,8})\}/.exec(src.slice(i + 1)); value += String.fromCodePoint(parseInt(m[1].replace(/_/g, ""), 16)); i += 1 + m[0].length; }
          else if (e === "\n" || e === "\r") { i += 2; while (/\s/.test(src[i] ?? "")) i++; }
          else throw new Error(`unknown escape \\${e}`);
        } else value += src[i++];
      }
      if (i >= src.length) throw new Error("unterminated string");
      i++;
      push(value, start);
      continue;
    }
    if (c === "'" || (c === "b" && src[i + 1] === "'" && !ident(src[i - 1]))) {
      const ch = /^b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]{1,8}\}|.)|[^\\'\n])'/u.exec(src.slice(i, i + 16));
      if (ch) { out += " "; i += ch[0].length; continue; }
    }
    out += c;
    i++;
  }
  flush();
  return { code: parts.join(""), strings };
}

/**
 * Removes comments from TOML and replaces each string with §n§ (keeping line breaks). Basic strings ("...") are
 * decoded with TOML escapes; literal strings ('...') are taken as written; multi-line strings ("""...""", '''...''')
 * are kept but marked `multiline` (callers report them instead of guessing). Anything malformed throws.
 */
export function lexToml(text) {
  const strings = [];
  let out = "";
  let i = 0;
  const push = (value, start, multiline) => {
    strings.push({ value, line: text.slice(0, start).split("\n").length, multiline });
    out += `§${strings.length - 1}§` + "\n".repeat(text.slice(start, i).split("\n").length - 1);
  };
  while (i < text.length) {
    const c = text[i];
    if (c === "#") {
      const end = text.indexOf("\n", i);
      i = end < 0 ? text.length : end;
    } else if (text.startsWith('"""', i) || text.startsWith("'''", i)) {
      const q = text.slice(i, i + 3);
      const start = i;
      const end = text.indexOf(q, i + 3);
      if (end < 0) throw new Error("unterminated multi-line string");
      let close = end + 3;
      while (text[close] === q[0] && close - end < 5) close++;
      i = close;
      // TOML trims a newline right after the opening delimiter; the value is not decoded further (callers report it).
      push(text.slice(start + 3, close - 3).replace(/^\r?\n/, ""), start, true);
    } else if (c === "'") {
      const start = i;
      const end = text.indexOf("'", i + 1);
      if (end < 0 || text.slice(i + 1, end).includes("\n")) throw new Error("unterminated literal string");
      i = end + 1;
      push(text.slice(start + 1, end), start, false);
    } else if (c === '"') {
      const start = i++;
      let value = "";
      for (;;) {
        const d = text[i];
        if (d === undefined || d === "\n") throw new Error("unterminated basic string");
        if (d === '"') { i++; break; }
        if (d !== "\\") { value += d; i++; continue; }
        const e = text[i + 1];
        const simple = { b: "\b", t: "\t", n: "\n", f: "\f", r: "\r", e: "\x1b", '"': '"', "\\": "\\" };
        if (e in simple) { value += simple[e]; i += 2; continue; }
        const hex = { x: 2, u: 4, U: 8 }[e];
        const digits = hex && text.slice(i + 2, i + 2 + hex);
        if (!hex || !new RegExp(`^[0-9A-Fa-f]{${hex}}$`).test(digits)) throw new Error(`unknown escape \\${e}`);
        value += String.fromCodePoint(parseInt(digits, 16));
        i += 2 + hex;
      }
      push(value, start, false);
    } else {
      out += c;
      i++;
    }
  }
  return { code: out, strings };
}

const IDENT = "(?:r#)?([A-Za-z_][A-Za-z0-9_]*)";

/**
 * The #[path] attributes in lexed Rust code: the literal (or null), what item follows (`file` for `mod x;`, `inline`
 * for `mod x { ... }`, `other`), and the inline modules around the attribute, outermost first, each with its own
 * #[path] attribute if it has one.
 */
export function pathAttributes(code, strings) {
  const attrs = [];
  for (const m of code.matchAll(/#\s*!?\s*\[[^\]]*?\bpath\s*=\s*/g)) {
    const lit = /^§(\d+)§/.exec(code.slice(m.index + m[0].length));
    const close = code.indexOf("]", m.index + m[0].length);
    const after = close < 0 ? "" : code.slice(close + 1, close + 1 + 400);
    const item = new RegExp(`^(?:\\s*#\\s*\\[[^\\]]*\\])*\\s*(?:pub\\s*(?:\\([^)]*\\))?\\s*)?(?:unsafe\\s+)?mod\\s+${IDENT}\\s*([;{])`).exec(after);
    attrs.push({
      index: m.index,
      literal: lit ? Number(lit[1]) : null,
      value: lit ? strings[Number(lit[1])].value : null,
      conditional: /^#\s*!?\s*\[\s*cfg_attr\b/.test(m[0]),
      on: !item ? "other" : item[2] === ";" ? "file" : "inline",
      brace: item && item[2] === "{" ? close + item[0].length : -1,
      chain: [],
    });
  }
  if (attrs.length === 0) return attrs;
  const onBrace = new Map(attrs.filter((a) => a.on === "inline").map((a) => [a.brace, a]));
  const byIndex = [...attrs].sort((a, b) => a.index - b.index);
  const stack = [];
  let k = 0;
  for (let i = 0; i < code.length && k < byIndex.length; i++) {
    while (k < byIndex.length && byIndex[k].index === i) byIndex[k++].chain = stack.filter(Boolean);
    if (code[i] === "{") {
      const mod = new RegExp(`\\bmod\\s+${IDENT}\\s*$`).exec(code.slice(Math.max(0, i - 120), i));
      stack.push(mod ? { name: mod[1], attr: onBrace.get(i) ?? null } : null);
    } else if (code[i] === "}") stack.pop();
  }
  return attrs;
}

/**
 * Where a #[path] attribute in `file` points (Rust Reference, "The path attribute", checked against rustc 1.100):
 *   - outside inline modules: relative to the file's directory, for every kind of file;
 *   - inside inline modules: each inline module adds its name as a directory, or its own #[path] value; a non-mod-rs
 *     file first adds its own name (file stem), unless the first inline module has a #[path].
 * Returns { target } for `mod x;`, { dir } for an inline module, or { unresolved: reason }.
 */
export function resolvePathAttribute(file, modRs, attr) {
  if (attr.value === null) return { unresolved: "not a string literal" };
  if (attr.on === "other") return { unresolved: "not on a module" };
  if (/^([\/\\]|[A-Za-z]:)/.test(attr.value)) return { unresolved: "absolute path" };
  let dir = posix.dirname(file);
  let stem = modRs ? null : posix.basename(file, ".rs");
  for (const m of attr.chain) {
    if (m.attr) {
      if (m.attr.value === null || m.attr.conditional) return { unresolved: "inside an inline module with a non-literal or conditional path" };
      dir = posix.join(dir, m.attr.value.replace(/\\/g, "/"));
    } else {
      if (stem) dir = posix.join(dir, stem);
      dir = posix.join(dir, m.name);
    }
    stem = null;
  }
  const target = posix.join(dir, attr.value.replace(/\\/g, "/"));
  if (target.startsWith("../") || target === "..") return { unresolved: "outside the repository" };
  return attr.on === "file" ? { target } : { dir: target };
}

/** Whether `file` is a crate root by Cargo's conventions (lib, main, bins, tests, examples, benches, build script). */
export function isCrateRoot(crate, file) {
  const rel = file.slice(crate.length + 1);
  return /^(src\/(lib|main)\.rs|build\.rs|src\/bin\/[^/]+\.rs|src\/bin\/[^/]+\/main\.rs|(tests|examples|benches)\/[^/]+\.rs|(tests|examples|benches)\/[^/]+\/main\.rs)$/.test(rel);
}

const lineAt = (code, index) => code.slice(0, index).split("\n").length;
const S = (n) => `§${n}§`;
const LIT = "§(\\d+)§";

/** Every file and directory under `dir` that git tracks or would track (gitignored outputs such as target/ excluded). */
function listFiles(root, dir) {
  const r = spawnSync("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", dir], { cwd: root, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ls-files ${dir} failed: ${r.stderr}`);
  return [...new Set(r.stdout.split("\0").filter(Boolean))].filter((f) => existsSync(path.join(root, f)));
}

/** Whether a repo-relative path is a file git tracks (or would track) or a directory holding one. */
function repoPaths(files) {
  const set = new Set();
  for (const file of files) {
    for (let p = file; p && p !== "."; p = posix.dirname(p)) {
      if (set.has(p)) break;
      set.add(p);
    }
  }
  return (p) => set.has(p);
}

/**
 * Scans the crates (directories relative to `root`). Returns
 *   inputs:     [{ crate, file, line, kind, target }]  every resolved reference (target is repo-relative, normalized);
 *   external:   the subset whose target is outside its crate;
 *   unresolved: [{ crate, file, line, kind, text }]     references whose target could not be determined.
 */
export function scanRustInputs(root, crates, options = {}) {
  // By default one `git ls-files` lists the repository (tracked and untracked, gitignored outputs such as target/ left
  // out); the crates' files and the existence checks come from that list.
  let repoFiles;
  const all = () => (repoFiles ??= listFiles(root, "."));
  const list = options.list ?? ((_, crate) => all().filter((f) => f.startsWith(`${crate}/`)));
  const exists = options.exists ?? repoPaths(all());
  const inputs = [];
  const unresolved = [];
  const repoRel = (p) => posix.normalize(p).replace(/\/$/, "");
  for (const crate of crates) {
    const files = list(root, crate);
    if (!files.includes(`${crate}/Cargo.toml`)) throw new Error(`${crate} has no Cargo.toml`);
    const add = (file, line, kind, target) => inputs.push({ crate, file, line, kind, target: repoRel(target) });
    const miss = (file, line, kind, text) => unresolved.push({ crate, file, line, kind, text: String(text).replace(/\s+/g, " ").slice(0, 160) });

    const targetRoots = new Set();
    for (const file of files.filter((f) => /(^|\/)Cargo\.toml$/.test(f) || /(^|\/)\.cargo\/config(\.toml)?$/.test(f))) {
      const text = readFileSync(path.join(root, file), "utf8");
      const base = file.endsWith("Cargo.toml") ? posix.dirname(file) : posix.dirname(posix.dirname(file));
      let lexed;
      try { lexed = lexToml(text); } catch (error) { miss(file, 0, "toml-lex-error", error.message); continue; }
      const { code, strings } = lexed;
      // `path`, `build` and `workspace` keys, bare, quoted or dotted (`foo.path = ...`), with any string form.
      for (const m of code.matchAll(/(?<=^|[\s{,])((?:[A-Za-z0-9_-]+|§\d+§)(?:[ \t]*\.[ \t]*(?:[A-Za-z0-9_-]+|§\d+§))*)[ \t]*=[ \t]*/g)) {
        const last = m[1].split(".").pop().trim();
        const name = /^§(\d+)§$/.test(last) ? strings[Number(last.slice(1, -1))].value : last;
        if (!["path", "build", "workspace"].includes(name)) continue;
        const rest = code.slice(m.index + m[0].length);
        const line = lineAt(code, m.index);
        const lit = /^§(\d+)§/.exec(rest);
        if (lit && !strings[Number(lit[1])].multiline) {
          const target = posix.join(base, strings[Number(lit[1])].value.replace(/\\/g, "/"));
          add(file, line, `toml-${name}`, target);
          if (name === "path" && target.endsWith(".rs")) targetRoots.add(repoRel(target));
        } else if (/^(true|false)\b/.test(rest) && name !== "path") continue;
        else miss(file, line, `toml-${name}`, `${m[0]}${rest.slice(0, 60)}`);
      }
      if (file.includes(".cargo/")) {
        for (const s of strings) if (/[\/\\]/.test(s.value)) miss(file, s.line, "cargo-config-path", s.value);
        if (/\brelative\s*=\s*true\b/.test(code)) miss(file, 0, "cargo-config-env", "relative = true");
      }
    }

    // Rust module resolution for #[path]: the file kind (mod-rs or not) decides where a path inside inline modules
    // points (Rust Reference, "The path attribute"; checked against rustc). Root modules and mod.rs files are mod-rs,
    // and so is a file loaded through #[path] (rustc behaviour; the Reference does not say). That last rule depends on
    // the attributes themselves, so it is applied until it settles; a file whose kind does not settle is reported.
    const sources = new Map();
    for (const file of files.filter((f) => f.endsWith(".rs"))) {
      try {
        const lexed = lexRust(readFileSync(path.join(root, file), "utf8"));
        sources.set(file, { ...lexed, attrs: pathAttributes(lexed.code, lexed.strings) });
      } catch (error) {
        miss(file, 0, "lex-error", error.message);
      }
    }
    const roots = new Set([...sources.keys()].filter((f) => isCrateRoot(crate, f) || posix.basename(f) === "mod.rs" || targetRoots.has(f)));
    const resolveAll = (modRs) => {
      const out = new Map();
      for (const [file, { attrs }] of sources) out.set(file, attrs.map((a) => resolvePathAttribute(file, modRs.has(file), a)));
      return out;
    };
    const loaded = (resolved) => new Set([...resolved.values()].flat().filter((r) => r.target).map((r) => repoRel(r.target)));
    let modRs = new Set(roots);
    let resolved = resolveAll(modRs);
    const unsettled = new Set();
    for (let round = 0; ; round++) {
      const next = new Set([...roots, ...loaded(resolved)]);
      const changed = [...next].filter((f) => !modRs.has(f)).concat([...modRs].filter((f) => !next.has(f)));
      if (changed.length === 0) break;
      if (round === 8) { for (const f of changed) unsettled.add(f); break; }
      modRs = next;
      resolved = resolveAll(modRs);
    }

    for (const [file, { code, strings, attrs }] of sources) {
      const dir = posix.dirname(file);
      const isBuild = posix.basename(file) === "build.rs" && dir === crate;
      const handled = new Set();
      const value = (n) => strings[Number(n)].value;

      // include_str! / include_bytes! / include!
      for (const m of code.matchAll(/\b(include_str|include_bytes|include)\s*!\s*[(\[{]/g)) {
        const rest = code.slice(m.index + m[0].length);
        const lit = new RegExp(`^\\s*${LIT}\\s*,?\\s*[)\\]}]`).exec(rest);
        const cat = new RegExp(`^\\s*concat\\s*!\\s*\\(\\s*env\\s*!\\s*\\(\\s*${LIT}\\s*\\)((?:\\s*,\\s*${LIT})*)\\s*,?\\s*\\)\\s*,?\\s*[)\\]}]`).exec(rest);
        if (lit) { handled.add(Number(lit[1])); add(file, lineAt(code, m.index), m[1], posix.join(dir, value(lit[1]))); }
        else if (cat && value(cat[1]) === "CARGO_MANIFEST_DIR") {
          const parts = [...cat[2].matchAll(/§(\d+)§/g)].map((p) => (handled.add(Number(p[1])), value(p[1]))).join("");
          handled.add(Number(cat[1]));
          add(file, lineAt(code, m.index), m[1], posix.join(crate, parts));
        } else miss(file, lineAt(code, m.index), m[1], m[0] + rest.slice(0, 120));
      }

      // #[path = "..."], also #[cfg_attr(..., path = "...")], resolved as rustc does (see above).
      attrs.forEach((a, k) => {
        const r = resolved.get(file)[k];
        const line = lineAt(code, a.index);
        if (a.literal !== null) handled.add(a.literal);
        if (r.unresolved) miss(file, line, "path-attribute", `${r.unresolved}: ${code.slice(a.index, a.index + 100)}`);
        else if (a.chain.length > 0 && unsettled.has(file)) miss(file, line, "path-attribute", `the module kind of ${file} does not settle`);
        else if (r.target) add(file, line, "path-attribute", r.target);
      });

      // CARGO_MANIFEST_DIR
      strings.forEach((s, n) => {
        if (s.value !== "CARGO_MANIFEST_DIR" || handled.has(n)) return;
        const at = code.indexOf(S(n));
        const before = code.slice(Math.max(0, at - 200), at);
        const after = code.slice(at + S(n).length);
        const line = lineAt(code, at);
        handled.add(n);
        const cat = new RegExp(`concat\\s*!\\s*\\(\\s*env\\s*!\\s*\\(\\s*$`).test(before) &&
          new RegExp(`^\\s*\\)((?:\\s*,\\s*${LIT})*)\\s*,?\\s*\\)`).exec(after);
        if (cat) {
          const parts = [...cat[1].matchAll(/§(\d+)§/g)].map((p) => (handled.add(Number(p[1])), value(p[1]))).join("");
          add(file, line, "manifest-dir", posix.join(crate, parts));
          return;
        }
        const wrapped = /(?:Path::new|PathBuf::from)\s*\(\s*(?:env\s*!|option_env\s*!)\s*\(\s*$/.test(before);
        const chain = new RegExp(`^\\s*\\)\\s*\\)((?:\\s*\\.\\s*join\\s*\\(\\s*${LIT}\\s*\\))+)`).exec(after);
        if (!wrapped || !chain) { miss(file, line, "manifest-dir", before.slice(-60) + S(n) + after.slice(0, 80)); return; }
        const segments = [...chain[1].matchAll(/§(\d+)§/g)].map((p) => (handled.add(Number(p[1])), value(p[1])));
        const target = posix.join(crate, ...segments);
        const next = after.slice(chain[0].length);
        if (/^\s*\.\s*(join|parent|ancestors|with_file_name|with_extension|push)\b/.test(next)) {
          miss(file, line, "manifest-dir-dynamic", `${target} then ${next.trim().slice(0, 60)}`);
        }
        add(file, line, "manifest-dir", target);
      });

      // build.rs: rerun-if-changed and reads, with literals or string constants.
      if (isBuild) {
        const consts = Object.fromEntries([...code.matchAll(new RegExp(`\\bconst\\s+([A-Z_][A-Z0-9_]*)\\s*:\\s*&(?:'static\\s+)?str\\s*=\\s*${LIT}`, "g"))].map((m) => [m[1], value(m[2])]));
        strings.forEach((s, n) => {
          const m = /^cargo::?rerun-if-changed=(.*)$/.exec(s.value);
          if (!m) return;
          handled.add(n);
          const resolved = m[1].replace(/\{([A-Z_][A-Z0-9_]*)\}/g, (all, name) => consts[name] ?? all);
          if (/[{}]/.test(resolved)) miss(file, s.line, "build-rerun", s.value);
          else add(file, s.line, "build-rerun", posix.join(crate, resolved));
        });
        for (const m of code.matchAll(/\b(?:fs::(?:read|read_to_string|read_dir|copy|metadata|canonicalize)|File::open)\s*\(\s*([^)]*)\)/g)) {
          const arg = m[1].trim().replace(/^&/, "");
          const lit = /^§(\d+)§$/.exec(arg);
          if (lit) { handled.add(Number(lit[1])); add(file, lineAt(code, m.index), "build-read", posix.join(crate, value(lit[1]))); }
          else if (arg in consts) add(file, lineAt(code, m.index), "build-read", posix.join(crate, consts[arg]));
          else miss(file, lineAt(code, m.index), "build-read", m[0]);
        }
      }

      // Any other literal that names an existing path outside the crate, or has a `..` segment.
      strings.forEach((s, n) => {
        if (handled.has(n)) return;
        const v = s.value;
        if (v === "" || v.length > 300 || /[\n\0]/.test(v) || /^[a-z][a-z0-9+.-]*:\/\//i.test(v)) return;
        const rel = v.replace(/\\/g, "/");
        const dotdot = rel.split("/").includes("..");
        if (!dotdot && !rel.includes("/") && !/\.[A-Za-z0-9]+$/.test(rel)) return;
        const tail = rel.replace(/^\/+/, "");
        const hits = new Set();
        for (const base of [dir, crate, ""]) {
          const candidate = repoRel(posix.join(base, tail));
          if (candidate === "" || candidate === "." || candidate.startsWith("../") || candidate === "..") continue;
          if (base === "" && candidate.startsWith(`${crate}/`)) continue;
          if (exists(candidate) && !(candidate === crate || candidate.startsWith(`${crate}/`))) hits.add(candidate);
        }
        for (const target of hits) add(file, s.line, "literal", target);
        if (dotdot && hits.size === 0) miss(file, s.line, "dotdot-literal", v);
      });
    }
  }
  const inside = (i) => i.target === i.crate || i.target.startsWith(`${i.crate}/`);
  return { inputs, external: inputs.filter((i) => !inside(i)), unresolved };
}

/** The crates the Rust CI job builds: native-host (the job's default directory) and every working-directory. */
export function ciCrates(root) {
  const ci = readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8").replace(/\r\n/g, "\n");
  const job = ci.slice(ci.indexOf("\n  rust:\n"));
  const end = job.slice(1).search(/\n  [\w-]+:\n/);
  const rust = end < 0 ? job : job.slice(0, end + 1);
  const dirs = [...rust.matchAll(/working-directory: ([\w-]+)/g)].map((m) => m[1]);
  return [...new Set(dirs)].sort();
}
