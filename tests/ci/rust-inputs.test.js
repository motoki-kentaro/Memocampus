// Issue #93 S3-2: every file a Rust crate reads from outside itself must make CI run the Rust job.
//
// scripts/ci/rust-inputs.mjs scans the crates the Rust job builds (a lexer and patterns, not a compiler; its header
// lists what it finds and what it cannot see). This test:
//   - checks every reference it finds against the change classifier (scripts/ci/classify-changes.mjs): a pull request
//     that changes one of those files must never skip Rust. A new include_str!("../../docs/x.md") fails here;
//   - pins the set of files outside the crates that they read, so that a new one is reviewed;
//   - pins the references the scanner cannot resolve to a reviewed list with the reason each one is not a file the
//     classifier lets through; a new unresolved reference fails until it is reviewed;
//   - runs the scanner on small synthetic crates: what it must find, what it must not mistake for a reference, and the
//     known blind spots (asserted, so that the list of limits stays true).
import { after, test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ciCrates, lexRust, scanRustInputs } from "../../scripts/ci/rust-inputs.mjs";
import { RUST_READS, skipsRust } from "../../scripts/ci/classify-changes.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CRATES = ["mcp-server", "native-host", "page-extract", "tray", "visible-store"];

/** The changed paths that would skip Rust although a crate reads them (a file, or anything in a directory). */
function uncovered(dir, scan) {
  const out = [];
  for (const { target, file, kind } of scan.inputs) {
    let isDir = false;
    try { isDir = statSync(path.join(dir, target)).isDirectory(); } catch {}
    const probes = isDir ? [`${target}/Cargo.toml`, `${target}/src/lib.rs`, `${target}/x.md`, `${target}/docs/x.md`] : [target];
    for (const p of probes) if (skipsRust(p)) out.push(`${p} (${kind} in ${file})`);
  }
  return out;
}

// ------------------------------------------------------------------ the real crates

const scan = scanRustInputs(root, ciCrates(root));

test("the scanned crates are exactly the crates the Rust CI job builds", () => {
  assert.deepEqual(ciCrates(root), CRATES);
});

test("no file a crate reads can be changed by a pull request that skips Rust", () => {
  assert.deepEqual(uncovered(root, scan), []);
  // Every file in a crate directory needs Rust, too (fixtures, data, resources, build scripts).
  const listed = spawnSync("git", ["ls-files", "-z", "--", ...CRATES], { cwd: root, encoding: "utf8" }).stdout.split("\0").filter(Boolean);
  assert.ok(listed.length > 100);
  assert.deepEqual(listed.filter(skipsRust), []);
});

test("the files outside the crates that they read are the reviewed ones", () => {
  const external = [...new Set(scan.external.map((e) => `${e.kind} ${e.target}`))].sort();
  assert.deepEqual(external, [
    // include_str!: product data (native-host/src/policy.rs) and test inputs.
    "include_str docs/issue-025-policy-engine.md",
    "include_str extension/policy-builtins.json",
    "include_str extension/policy-defaults.json",
    "include_str tests/page-metadata/vectors.json",
    "include_str tests/privacy/vectors.json",
    // Cargo path dependencies (mcp-server -> visible-store, tray, native-host; visible-store -> native-host).
    "toml-path native-host",
    "toml-path tray",
    "toml-path visible-store",
  ]);
  // The only Rust input in an otherwise allowed directory is on the classifier's deny list.
  const inAllowedDirs = scan.external.map((e) => e.target).filter((t) => /^(docs|tests\/docs)\//.test(t) || /^README(\.en)?\.md$/.test(t));
  assert.deepEqual([...new Set(inAllowedDirs)], RUST_READS);
});

test("references found inside the crates include the known ones (the scanner sees them)", () => {
  const has = (kind, target) => scan.inputs.some((i) => i.kind === kind && i.target === target);
  assert.ok(has("include_str", "native-host/data/public_suffix_list.dat"));
  assert.ok(has("include_str", "visible-store/src/snapshot.rs"));
  assert.ok(has("build-read", "native-host/resources/memocampus.ico"));
  assert.ok(has("build-rerun", "native-host/resources/memocampus.ico"));
  assert.ok(has("build-rerun", "native-host/build.rs"));
  assert.ok(has("manifest-dir", "native-host/resources/memocampus.ico"));
  assert.ok(has("manifest-dir", "visible-store/src"));
  assert.ok(has("manifest-dir", "page-extract/tests/fixtures/chrome"));
  assert.ok(has("toml-path", "mcp-server/src/main.rs"));
});

/** References the scanner cannot resolve, each reviewed: none of them names a file outside its crate. */
const REVIEWED = [
  ["native-host/src/images/tests.rs", "dotdot-literal", "bad id/..", "an invalid image save id the parser must refuse"],
  ["native-host/src/item_delete/tests.rs", "dotdot-literal", "..", "backup_dir.join(\"..\"): a temporary directory's parent"],
  ["native-host/src/page_manage/export/tests.rs", "dotdot-literal", "..\\..\\Windows\\System32\\evil", "a hostile export file name to sanitize"],
  ["native-host/src/page_manage/export/tests.rs", "dotdot-literal", "../../etc/passwd", "a hostile export file name to sanitize"],
  ["native-host/src/page_manage/export/tests.rs", "dotdot-literal", "..", "out.join(\"..\"): an export target that must be refused"],
  ["native-host/src/policy.rs", "dotdot-literal", "..", "the `..` URL path segment in a match arm (URL normalization)"],
  ["native-host/src/policy/tests.rs", "dotdot-literal", "example.com/../*", "a URL rule pattern"],
  ["native-host/src/policy/tests.rs", "dotdot-literal", "/x/../private/a", "a URL path"],
  ["native-host/src/policy/tests.rs", "dotdot-literal", "example.com/a/../b", "a URL rule pattern"],
  ["native-host/src/policy/tests.rs", "dotdot-literal", "example.com/a/*.png/..", "a URL rule pattern"],
  ["native-host/src/secure_vault/container/tests.rs", "dotdot-literal", "../../etc", "a hostile vault id the container must refuse"],
  ["native-host/src/secure_vault/container/tests.rs", "dotdot-literal", "../evil", "a hostile section kind the container must refuse"],
  ["page-extract/tests/integration.rs", "manifest-dir-dynamic", "page-extract/tests/fixtures/chrome then .join(name)", "fixture(name): names are literals in this crate (checked below)"],
  ["page-extract/tests/quality.rs", "manifest-dir-dynamic", "page-extract/tests/fixtures/chrome then .join(name)", "fixture(name): names are literals in this crate (checked below)"],
  ["page-extract/tests/tokenizer.rs", "manifest-dir-dynamic", "page-extract/tests/fixtures/chrome then .join(name)", "fixture(name): names are literals in this crate (checked below)"],
  ["tray/tests/ui_process.rs", "dotdot-literal", "..\\x", "an isolated registry name that must be refused"],
];

test("every reference the scanner cannot resolve has been reviewed, and no reviewed one has gone stale", () => {
  const key = (file, kind, text) => `${file} | ${kind} | ${text}`;
  const found = scan.unresolved.map((u) => {
    const reviewed = REVIEWED.find(([file, kind, text]) => file === u.file && kind === u.kind && (u.text === text || u.text.startsWith(text)));
    return reviewed ? key(...reviewed.slice(0, 3)) : `NOT REVIEWED: ${key(u.file, u.kind, u.text)} (line ${u.line})`;
  });
  assert.deepEqual(found.sort(), REVIEWED.map((r) => key(...r.slice(0, 3))).sort());
});

test("page-extract's fixture(name) is only called with literal names of files inside the crate", () => {
  for (const file of ["integration.rs", "quality.rs", "tokenizer.rs"]) {
    const { code, strings } = lexRust(readFileSync(path.join(root, "page-extract/tests", file), "utf8"));
    const calls = [...code.matchAll(/\bfixture\s*\(([^)]*)\)/g)].filter((m) => !/^\s*name\s*:/.test(m[1]));
    assert.ok(calls.length > 0, file);
    for (const m of calls) {
      const lit = /^\s*§(\d+)§\s*$/.exec(m[1]);
      assert.ok(lit, `${file}: fixture(${m[1].trim()}) is not a literal`);
      const name = strings[Number(lit[1])].value;
      assert.match(name, /^[\w.-]+$/, `${file}: ${name}`);
      assert.ok(statSync(path.join(root, "page-extract/tests/fixtures/chrome", name)).isFile(), name);
    }
  }
});

// ------------------------------------------------------------------ synthetic crates

/** A scratch repository with a crate `c` (Cargo.toml, given files) and existing files outside it. */
// The files are listed by walking the directory (no git process per case: the suite runs in parallel with UI tests
// that wait for fixed times). The real crates above are listed by git, which leaves out ignored outputs.
const walk = (dir, crate) => readdirSync(path.join(dir, crate), { recursive: true, withFileTypes: true })
  .filter((e) => e.isFile()).map((e) => path.relative(dir, path.join(e.parentPath ?? e.path, e.name)).split(path.sep).join("/"));
const sandboxes = [];
after(() => { for (const dir of sandboxes) rmSync(dir, { recursive: true, force: true }); });
function sandbox(files, outside = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), "mc-rust-inputs-"));
  sandboxes.push(dir);
  const all = { "c/Cargo.toml": '[package]\nname = "c"\n', "c/src/snapshot.rs": "", "c/tests/fixtures/f.bin": "", ...files, ...outside };
  for (const [p, text] of Object.entries(all)) {
    mkdirSync(path.dirname(path.join(dir, p)), { recursive: true });
    writeFileSync(path.join(dir, p), text);
  }
  return { scan: scanRustInputs(dir, ["c"], { list: walk, exists: (p) => existsSync(path.join(dir, p)) }), dir };
}
const DOCS = Object.fromEntries("abcdefghijklmnopq".split("").map((x) => [`docs/${x}.md`, "x"]));
const targets = (s) => s.external.map((e) => e.target).sort();
const kinds = (s) => s.external.filter((e) => e.kind !== "literal").map((e) => `${e.kind} ${e.target}`).sort();
const unresolvedKinds = (s) => s.unresolved.map((u) => u.kind).sort();

test("a new docs file read with include_str! is found and would let a docs-only PR skip Rust (the real test fails)", () => {
  const { scan: s, dir } = sandbox({ "c/src/lib.rs": 'pub const A: &str = include_str!("../../docs/a.md");\n' }, DOCS);
  assert.deepEqual(targets(s), ["docs/a.md"]);
  assert.deepEqual(uncovered(dir, s), ["docs/a.md (include_str in c/src/lib.rs)"]);
});

test("a new JSON read with include_bytes! or include_str! is found (also when it sits in docs/)", () => {
  const { scan: s, dir } = sandbox({
    "c/src/lib.rs": 'const J: &[u8] = include_bytes!("../../tests/new/vectors.json");\nconst K: &str = include_str! (\n  "../../docs/data.json"\n);\n',
  }, { "tests/new/vectors.json": "{}", "docs/data.json": "{}" });
  assert.deepEqual(targets(s), ["docs/data.json", "tests/new/vectors.json"]);
  assert.deepEqual(uncovered(dir, s), ["docs/data.json (include_str in c/src/lib.rs)"]);
  // Found as the macro itself, not only by the literal fallback.
  assert.deepEqual(kinds(s), ["include_bytes tests/new/vectors.json", "include_str docs/data.json"]);
});

test("include!, #[path] and cfg_attr(path) are found", () => {
  const { scan: s } = sandbox({
    "c/src/lib.rs": 'include!("../../docs/b.md");\n#[path = "../../docs/c.md"]\nmod x;\n#[cfg_attr(windows, path = "../../docs/d.md")]\nmod y;\n',
  }, DOCS);
  assert.deepEqual(targets(s), ["docs/b.md", "docs/c.md", "docs/d.md"]);
  assert.deepEqual(kinds(s), ["include docs/b.md", "path-attribute docs/c.md", "path-attribute docs/d.md"]);
});

test("a file the lexer cannot read is reported, never skipped", () => {
  const { scan: s } = sandbox({ "c/src/lib.rs": 'const A: &str = "never closed;\n', "c/src/ok.rs": 'const B: &str = include_str!("../../docs/a.md");\n' }, DOCS);
  assert.deepEqual(unresolvedKinds(s), ["lex-error"]);
  assert.deepEqual(targets(s), ["docs/a.md"]);
});

test("CARGO_MANIFEST_DIR with literal joins or concat! is resolved; any other use is unresolved, never safe", () => {
  const { scan: s } = sandbox({
    "c/tests/t.rs": [
      'fn a() { std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("docs/e.md")).unwrap(); }',
      'const B: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/f.md"));',
      'fn c() { std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../", "docs/g.md")).unwrap(); }',
      'fn d() { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join("f.bin"); }',
      'fn e(name: &str) { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join(name); }',
      'fn f() { let root = env!("CARGO_MANIFEST_DIR"); }',
      'fn g() { std::env::var("CARGO_MANIFEST_DIR").unwrap(); }',
      'fn h() { Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("docs/h.md"); }',
    ].join("\n"),
  }, DOCS);
  assert.deepEqual(targets(s), ["docs/e.md", "docs/f.md", "docs/g.md", "docs/h.md"]);
  assert.ok(s.inputs.some((i) => i.target === "c/tests/fixtures/f.bin" && i.kind === "manifest-dir"));
  assert.deepEqual(unresolvedKinds(s), ["manifest-dir", "manifest-dir", "manifest-dir", "manifest-dir-dynamic"]);
});

test("build.rs: rerun-if-changed and reads through constants are found; a read of a variable is unresolved", () => {
  const { scan: s } = sandbox({
    "c/build.rs": [
      'const DOC: &str = "../docs/i.md";',
      'fn main() {',
      '  println!("cargo:rerun-if-changed={DOC}");',
      '  println!("cargo::rerun-if-changed=../docs/j.md");',
      '  let _ = std::fs::read_to_string(DOC);',
      '  let p = std::env::var("X").unwrap();',
      '  let _ = fs::read(&p);',
      '}',
    ].join("\n"),
  }, DOCS);
  assert.deepEqual([...new Set(targets(s))], ["docs/i.md", "docs/j.md"]);
  assert.deepEqual(kinds(s), ["build-read docs/i.md", "build-rerun docs/i.md", "build-rerun docs/j.md"]);
  assert.deepEqual(unresolvedKinds(s), ["build-read"]);
});

test("Cargo path dependencies and paths in .cargo/config.toml are reported", () => {
  const { scan: s, dir } = sandbox({
    "c/Cargo.toml": '[package]\nname = "c"\n[dependencies]\nother = { path = "../other" }\n# old = { path = "../docs" }\n',
    "c/.cargo/config.toml": '[env]\nDATA = { value = "../docs/k.md", relative = true }\n',
  }, { "other/Cargo.toml": "", ...DOCS });
  assert.deepEqual(targets(s), ["other"]);
  assert.deepEqual(unresolvedKinds(s).sort(), ["cargo-config-env", "cargo-config-path"]);
  assert.deepEqual(uncovered(dir, s), []);
});

test("Cargo.toml: path, build and workspace in basic, literal, quoted-key and dotted-key forms; comments ignored", () => {
  const { scan: s } = sandbox({
    "c/Cargo.toml": [
      "[package]",
      'name = "c"',
      "build = '../docs/build.rs'      # a literal string: no escapes",
      'workspace = "../ws"',
      "[dependencies]",
      'a = { path = "../da" }',
      "b = { path = '../db' }",
      'c2 = { "path" = "../dc" }',
      "d.path = '../dd'",
      'e = { path = "..\\\\de" }',
      "f = { path = '..\\de2' }",
      "# g = { path = '../docs/commented' }",
      'h = { version = "1", workspace = true }',
      "[target.'cfg(windows)'.dependencies]",
      "w = { path = '../dw' }",
    ].join("\n"),
  }, { "docs/build.rs": "", "ws/x": "", "da/x": "", "db/x": "", "dc/x": "", "dd/x": "", "de/x": "", "de2/x": "", "dw/x": "", "docs/commented/x": "" });
  assert.deepEqual(kinds(s), ["toml-build docs/build.rs", "toml-path da", "toml-path db", "toml-path dc", "toml-path dd", "toml-path de", "toml-path de2", "toml-path dw", "toml-workspace ws"]);
  assert.deepEqual(s.unresolved, []);
});

test("Cargo.toml: multi-line strings, non-string values and malformed TOML are unresolved, never skipped", () => {
  const cases = [
    ["a = { path = '''../da''' }", "toml-path"],
    ['a = { path = """../da""" }', "toml-path"],
    ['a = { path = ["../da"] }', "toml-path"],
    ["[package]\nbuild = '''../docs/b.rs'''", "toml-build"],
    ['a = { path = "../da }', "toml-lex-error"],
    ["a = { path = '../da }", "toml-lex-error"],
    ['a = { path = "\\q" }', "toml-lex-error"],
  ];
  for (const [toml, kind] of cases) {
    const { scan: s } = sandbox({ "c/Cargo.toml": `[package]\nname = "c"\n[dependencies]\n${toml}\n` }, { "da/x": "" });
    assert.deepEqual(unresolvedKinds(s), [kind], toml);
    assert.deepEqual(targets(s), [], toml);
  }
  // build = false and workspace = true are not paths.
  const { scan: s } = sandbox({ "c/Cargo.toml": '[package]\nname = "c"\nbuild = false\n[dependencies]\nx = { workspace = true }\n' });
  assert.deepEqual(s.unresolved, []);
});

test(".cargo/config.toml: paths in basic, literal and multi-line strings are all reported", () => {
  const { scan: s } = sandbox({
    "c/.cargo/config.toml": "[env]\nA = '../docs/a.md'\nB = \"..\\\\docs\\\\b.md\"\nC = '''\n../docs/c.md'''\n# D = '../docs/d.md'\n",
  }, DOCS);
  assert.deepEqual(s.unresolved.map((u) => u.text), ["../docs/a.md", "..\\docs\\b.md", "../docs/c.md"]);
});

test("#[path] is resolved like rustc: root, main, mod.rs, non-mod-rs files, nested and named inline modules, path-loaded files", () => {
  // Every expected location below is where rustc 1.100 looked for the module (the S3 review's rustc experiment).
  const { scan: s } = sandbox({
    "c/src/lib.rs": [
      'mod inline1 { #[path = "other.rs"] mod inner; }',
      '#[path = "../../docs/a.md"] mod doc;',
      '#[path = "p/q.rs"] mod q;',
      'mod outer { mod mid { #[path = "n.rs"] mod deep; } }',
      '#[path = "thread_files"] mod thread { #[path = "tls.rs"] mod local_data; }',
      '#[cfg_attr(windows, path = "ca.rs")] mod ca;',
      "mod a;",
      "mod foo;",
    ].join("\n"),
    "c/src/main.rs": 'mod m { #[path = "mm.rs"] mod x; }\n',
    "c/src/a/mod.rs": 'mod inl { #[path = "z.rs"] mod inner; }\n',
    "c/src/foo.rs": [
      '#[path = "x.rs"] mod top;',
      'mod inl { #[path = "y.rs"] mod inner; }',
      '#[path = "tf"] mod t { #[path = "u.rs"] mod u; }',
      'mod x { #[path = "tf"] mod t { #[path = "u.rs"] mod u; } }',
      'pub(crate) mod r#try { #[path = "k.rs"] mod k; }',
      'mod y { mod z { #[path = "../../../../../docs/b.md"] mod w; } }',
    ].join("\n"),
    "c/src/p/q.rs": 'mod inl { #[path = "w.rs"] mod inner; }\n',
    "c/tests/t.rs": 'mod inl { #[path = "tt.rs"] mod x; }\n',
  }, DOCS);
  const found = s.inputs.filter((i) => i.kind === "path-attribute").map((i) => `${i.file} -> ${i.target}`).sort();
  assert.deepEqual(found, [
    "c/src/a/mod.rs -> c/src/a/inl/z.rs", // mod.rs: inline modules only
    "c/src/foo.rs -> c/src/foo/inl/y.rs", // non-mod-rs: its own name first
    "c/src/foo.rs -> c/src/foo/try/k.rs", // raw identifier
    "c/src/foo.rs -> c/src/foo/x/tf/u.rs", // own name, inline name, then the inline module's path
    "c/src/foo.rs -> c/src/tf/u.rs", // an inline module with a path replaces the file's own name
    "c/src/foo.rs -> c/src/x.rs", // outside inline modules: the file's directory, also for non-mod-rs
    "c/src/foo.rs -> docs/b.md",
    "c/src/lib.rs -> c/src/ca.rs",
    "c/src/lib.rs -> c/src/inline1/other.rs",
    "c/src/lib.rs -> c/src/outer/mid/n.rs",
    "c/src/lib.rs -> c/src/p/q.rs",
    "c/src/lib.rs -> c/src/thread_files/tls.rs",
    "c/src/lib.rs -> docs/a.md",
    "c/src/main.rs -> c/src/m/mm.rs",
    "c/src/p/q.rs -> c/src/p/inl/w.rs", // loaded through #[path]: treated as mod-rs by rustc
    "c/tests/t.rs -> c/tests/inl/tt.rs", // an integration test is a crate root
  ]);
  assert.deepEqual(targets(s), ["docs/a.md", "docs/b.md"]);
  assert.deepEqual(s.unresolved, []);
});

test("#[path] that cannot be resolved is reported: non-literal, absolute, conditional inline path, outside the repository, not on a module", () => {
  const { scan: s } = sandbox({
    "c/src/lib.rs": [
      '#[path = concat!("a", ".rs")] mod n1;',
      '#[path = "/abs/x.rs"] mod n2;',
      '#[path = "C:\\\\x.rs"] mod n3;',
      '#[cfg_attr(unix, path = "cu")] mod cm { #[path = "v.rs"] mod v; }',
      '#[path = "../../../outside.rs"] mod n4;',
      '#[path = "x.rs"] fn f() {}',
    ].join("\n"),
  });
  const reasons = s.unresolved.filter((u) => u.kind === "path-attribute").map((u) => u.text.split(":")[0]).sort();
  assert.deepEqual(reasons, ["absolute path", "absolute path", "inside an inline module with a non-literal or conditional path", "not a string literal", "not on a module", "outside the repository"]);
});

test("plain string literals naming a file outside the crate are found; `..` literals naming nothing are unresolved", () => {
  const { scan: s } = sandbox({
    "c/src/lib.rs": [
      'fn a() { std::fs::read_to_string("../docs/l.md"); }',
      'fn b() { std::fs::read(r#"../../docs/m.md"#); }',
      'fn c() { std::fs::read("..\\\\docs\\\\n.md"); }',
      'fn d() { std::fs::read("docs/o.md"); }',
      'fn e() { let bad = "../../etc/passwd"; }',
    ].join("\n"),
  }, DOCS);
  assert.deepEqual(targets(s), ["docs/l.md", "docs/m.md", "docs/n.md", "docs/o.md"]);
  assert.deepEqual(unresolvedKinds(s), ["dotdot-literal"]);
});

test("references inside the crate are inputs but not external", () => {
  const { scan: s } = sandbox({
    "c/src/lib.rs": 'const S: &str = include_str!("snapshot.rs");\nfn f() { std::fs::read("tests/fixtures/f.bin"); }\n',
  });
  assert.deepEqual(s.external, []);
  // Macros and attributes are recorded wherever they point; a plain literal only when it names something outside.
  assert.deepEqual(s.inputs.map((i) => i.target).sort(), ["c/src/snapshot.rs"]);
  assert.deepEqual(s.unresolved, []);
});

test("comments are not references, and strings, chars and lifetimes do not confuse the lexer", () => {
  const { scan: s } = sandbox({
    "c/src/lib.rs": [
      '// include_str!("../../docs/p.md")',
      '/* outer /* inner */ include_str!("../../docs/p.md") */',
      '/// See docs/p.md and ../../docs/p.md',
      'const U: &str = "https://example.com//a"; // not a comment start inside the string',
      "fn f<'a>(x: &'a str) -> char { if x.is_empty() { '\"' } else { '\\'' } }",
      'const Q: &str = "a \\" quote // still a string";',
      'const R: &str = include_str!("../../docs/q.md");',
    ].join("\n"),
  }, DOCS);
  assert.deepEqual(targets(s), ["docs/q.md"]);
  assert.deepEqual(s.unresolved, []);
});

test("an include whose argument is not a literal is unresolved (OUT_DIR, a constant, a macro_rules! parameter)", () => {
  const { scan: s } = sandbox({
    "c/src/lib.rs": [
      'include!(concat!(env!("OUT_DIR"), "/gen.rs"));',
      'const P: &str = "x"; const T: &str = include_str!(P);',
      'macro_rules! inc { ($p:literal) => { include_str!($p) }; }',
      'const V: &str = inc!("../../docs/a.md");',
    ].join("\n"),
  }, DOCS);
  assert.deepEqual(unresolvedKinds(s), ["include", "include_str", "include_str"]);
  // The macro's call site still names an existing file outside the crate, so it is found as a literal.
  assert.deepEqual(targets(s), ["docs/a.md"]);
});

test("known blind spots: paths built at run time from values the scanner cannot read are NOT found", () => {
  // Asserted so that the documented limits stay true; each of these needs a reviewer, not the scanner.
  const { scan: s } = sandbox({
    "c/src/lib.rs": [
      'fn a(root: &str) { std::fs::read(format!("{root}/docs/b.md")); }',
      'fn b() { std::fs::read(std::env::var("DOCS_FILE").unwrap()); }',
      'fn c() { std::env::current_dir().unwrap().parent().unwrap().join("docs").join("c"); }',
      'fn d() { std::process::Command::new("node").arg("read-docs.mjs"); }',
    ].join("\n"),
  }, DOCS);
  assert.deepEqual(s.external, []);
  assert.deepEqual(s.unresolved, []);
});

test("the lexer decodes escapes and keeps line numbers", () => {
  const { code, strings } = lexRust('let a = "x\\ty\\u{41}\\x42";\nlet b = b"q";\nlet c = r##"a"#b"##;\nlet d = "multi\nline";\nlet e = include_str!("f");\n');
  assert.deepEqual(strings.map((s) => s.value), ["x\tyAB", "q", 'a"#b', "multi\nline", "f"]);
  assert.deepEqual(strings.map((s) => s.line), [1, 2, 3, 4, 6]);
  assert.equal(code.split("\n").length, 7);
  assert.throws(() => lexRust('let a = "open'));
});
