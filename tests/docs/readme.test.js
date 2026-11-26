// The user READMEs (Issue #86): README.md (Japanese) and README.en.md (English); docs/development.md for links.
//
// What this checks: the two languages have the same structure; links and anchors resolve; quoted screen labels
// are the extension's and the tray's own text; the distribution status says "under review, not public" and no
// known wording claims otherwise; specification numbers agree with the code's constants; and the safety
// statements that users rely on are present, in the section they belong to.
//
// What it cannot check: whether a sentence is true. A statement can be reversed in words the patterns below do
// not know, or a wrong sentence can be added next to a right one, and this still passes. The patterns are short
// key phrases, deliberately loose so that rewording does not break them; they prove presence, not meaning.
// Whether the READMEs describe the product correctly, and say the same thing in both languages, needs a person's
// review whenever they or the product change.
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (file) => readFileSync(path.join(root, file), "utf8");
const JA = "README.md";
const EN = "README.en.md";
const DEV = "docs/development.md";
const docs = { [JA]: read(JA), [EN]: read(EN), [DEV]: read(DEV) };
const catalog = (lang) => JSON.parse(read(`extension/_locales/${lang}/messages.json`));
const ja = catalog("ja");
const en = catalog("en");

/** Text outside fenced code blocks. */
const prose = (md) => md.replace(/^```[\s\S]*?^```/gm, "");
/** GitHub's heading anchor: lower case, punctuation removed, spaces to hyphens (no duplicate suffixes). */
const slug = (heading) => heading.trim().toLowerCase().replace(/[^\p{L}\p{N}\p{M}\s_-]/gu, "").replace(/\s/g, "-");
const headings = (md) => [...prose(md).matchAll(/^(#{1,6}) (.+)$/gm)].map((m) => ({ level: m[1].length, text: m[2] }));
/** The text of the numbered `## n.` section, up to the next `## `. */
function section(md, n) {
  const start = md.search(new RegExp(`^## ${n}\\. `, "m"));
  assert.ok(start >= 0, `section ${n} exists`);
  const rest = md.slice(start + 3);
  const end = rest.search(/^## /m);
  return end < 0 ? rest : rest.slice(0, end);
}
/** The text before the first `## ` (the introduction with the status notice). */
const intro = (md) => md.slice(0, md.search(/^## /m));
/** Per `## ` section: its number, how many `### ` it has, and the sequence of its alert types. */
function sections(md) {
  const out = [{ number: 0, h3: 0, alerts: [] }];
  for (const line of prose(md).split("\n")) {
    const h2 = /^## (\d+)\. /.exec(line);
    if (h2) out.push({ number: Number(h2[1]), h3: 0, alerts: [] });
    else if (/^## /.test(line)) out.push({ number: null, h3: 0, alerts: [] });
    else if (/^### /.test(line)) out.at(-1).h3 += 1;
    const alert = /^> \[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]$/.exec(line);
    if (alert) out.at(-1).alerts.push(alert[1]);
  }
  return out;
}
const all = (md, re) => [...md.matchAll(re)].map((m) => m[1]);
/** Linked badge images `[![alt](image)](link)`, in order (Issue #96). */
const BADGE = /\[!\[([^\]]*)\]\(([^)\s]+)\)\]\(([^)\s]+)\)/g;
const badges = (md) => [...md.matchAll(BADGE)].map(([, alt, image, link]) => ({ alt, image, link }));

test("both READMEs show the same five badges after the language switch: live CI status, then four technologies", () => {
  for (const md of [docs[JA], docs[EN]]) {
    assert.deepEqual(badges(md).map((b) => b.alt), ["CI", "Rust Native", "MCP Server", "Chrome MV3", "SQLCipher"]);
    // One paragraph (rendered side by side) right after the language switch, and no other image anywhere.
    assert.match(intro(md), /^.*\]\(README(\.en)?\.md\).*\n\n(?:\[!\[[^\n]+\)\n){5}\n[^\n[]/m);
    assert.equal((md.match(/!\[/g) ?? []).length, 5);
    assert.doesNotMatch(md, /<img\b/i);
  }
  assert.deepEqual(badges(docs[EN]), badges(docs[JA]));
  // CI: GitHub's own badge for the real workflow on main, so it shows whatever the latest run says.
  const [ci, ...stack] = badges(docs[JA]);
  assert.ok(existsSync(path.join(root, ".github/workflows/ci.yml")));
  assert.equal(ci.image, "https://github.com/motoki-kentaro/Memocampus/actions/workflows/ci.yml/badge.svg?branch=main");
  assert.equal(ci.link, "https://github.com/motoki-kentaro/Memocampus/actions/workflows/ci.yml?query=branch%3Amain");
  // The four technologies: static Shields.io badges whose text is exactly their name, all flat-square, each linking
  // to that technology's official page.
  const OFFICIAL = {
    "Rust Native": "https://www.rust-lang.org/",
    "MCP Server": "https://modelcontextprotocol.io/",
    "Chrome MV3": "https://developer.chrome.com/docs/extensions/develop/migrate/what-is-mv3",
    "SQLCipher": "https://www.zetetic.net/sqlcipher/",
  };
  assert.deepEqual(stack.map((b) => [b.alt, b.link]), Object.entries(OFFICIAL));
  for (const b of stack) {
    const m = /^https:\/\/img\.shields\.io\/badge\/([^?/]+)-[0-9A-F]{6}\?(.+)$/.exec(b.image);
    assert.ok(m, `${b.alt}: ${b.image}`);
    assert.equal(decodeURIComponent(m[1]), b.alt);
    assert.match(m[2], /(^|&)style=flat-square(&|$)/);
  }
  // No badge states a result, a certification, a release, a platform, a license or other technologies.
  for (const b of badges(docs[JA])) {
    assert.doesNotMatch(`${b.alt} ${decodeURIComponent(b.image)}`,
      /passing|passed|success|failing|tests?\b|certified|verified|secure|official|release|store|windows|x64|platform|license|local-first|node|typescript|\bv\d/i);
  }
});

test("both READMEs open with the language switch to each other", () => {
  const firstLines = (md) => md.split("\n").slice(0, 4).join("\n");
  assert.match(firstLines(docs[JA]), /^\*\*日本語\*\* \| \[English\]\(README\.en\.md\)$/m);
  assert.match(firstLines(docs[EN]), /^\[日本語\]\(README\.md\) \| \*\*English\*\*$/m);
});

test("the Japanese and English READMEs have the same twelve numbered sections, subsections and alerts", () => {
  const jaSections = sections(docs[JA]);
  const enSections = sections(docs[EN]);
  assert.deepEqual(jaSections.filter((s) => s.number > 0).map((s) => s.number), [...Array(12)].map((_, i) => i + 1));
  assert.deepEqual(enSections, jaSections);
  // Section 5 covers the four kinds of record named by the agreed structure (by name, not by exact title).
  for (const md of [docs[JA], docs[EN]]) {
    const titles = headings(section(md, 5)).filter((h) => h.level === 3).map((h) => h.text);
    for (const name of ["URL", "History", "Image", "Page"]) assert.ok(titles.some((t) => t.includes(name)), `section 5 has ${name}`);
  }
});

test("every relative link and anchor resolves", () => {
  for (const [file, md] of Object.entries(docs)) {
    for (const [, target] of prose(md).matchAll(/\]\(([^)\s]+)\)/g)) {
      if (/^https?:\/\//.test(target)) continue;
      const [rel, anchor] = target.split("#");
      const resolved = rel ? path.join(path.dirname(path.join(root, file)), rel) : path.join(root, file);
      assert.ok(existsSync(resolved), `${file}: ${target} does not exist`);
      if (anchor !== undefined) {
        const anchors = headings(readFileSync(resolved, "utf8")).map((h) => slug(h.text));
        assert.ok(anchors.includes(decodeURIComponent(anchor)), `${file}: #${anchor} is not a heading of ${rel || file}`);
      }
    }
  }
});

test("each README links the privacy policy in its own language", () => {
  assert.ok(docs[JA].includes("https://memocampus.github.io/privacy/"));
  assert.ok(!docs[JA].includes("https://memocampus.github.io/en/privacy/"));
  assert.ok(docs[EN].includes("https://memocampus.github.io/en/privacy/"));
  assert.ok(!docs[EN].includes("https://memocampus.github.io/privacy/"));
});

test("the distribution status is 'under review, not public', and nothing presents another as available", () => {
  // The status notice before section 1, and the unconfirmed items in section 3.
  assert.match(intro(docs[JA]), /審査中/);
  assert.match(intro(docs[JA]), /まだ一般公開していません|公開前/);
  assert.match(intro(docs[EN]), /under review/);
  assert.match(intro(docs[EN]), /not been (released|published)|not (yet )?(released|published)/);
  assert.match(section(docs[JA], 3), /未確認/);
  assert.match(section(docs[EN], 3), /[Nn]ot confirmed/);
  // Known wordings that would claim publication or approval. A claim in other words is not caught.
  const claims = {
    [JA]: [/公開済み/, /承認済みです/, /審査に(通り|合格し)ました/, /公開しました/, /配信中/, /すべて確認済み/],
    [EN]: [/has been (approved|published|released)/i, /(?<!\b(once|until|when|after|before|if) it )\bis (now )?(available|published) (in|on|from) the Chrome Web Store/i,
      /\bnow (available|published)\b/i, /approved (in|by) the Chrome Web Store/i, /\ball (of it )?(has been )?confirmed\b/i],
  };
  for (const [file, patterns] of Object.entries(claims)) {
    for (const re of patterns) assert.doesNotMatch(docs[file], re, `${file} claims availability: ${re}`);
  }
  for (const file of [JA, EN]) {
    const md = docs[file];
    // The Store listing is added after publication; the review pre-release is not mentioned for users at all.
    for (const banned of ["chromewebstore.google.com", "chrome.google.com/webstore", "cdklofjfcjagkaclekmicliailmlnfla", "review.1",
      "/releases/", "プレリリース", "pre-release", "prerelease"]) {
      assert.ok(!md.toLowerCase().includes(banned), `${file} must not contain ${banned}`);
    }
    // The badges' own URLs are checked exactly by the badge test above; only the badge markup itself is left out here,
    // so the same URLs anywhere else are still unexpected.
    const urls = [...md.replace(BADGE, "").matchAll(/https?:\/\/[^\s)>`]+/g)].map((m) => m[0]);
    const allowed = [/^https:\/\/memocampus\.github\.io\/(en\/)?privacy\/$/, /^http:\/\/127\.0\.0\.1:\d+\/mcp$/];
    for (const url of urls) assert.ok(allowed.some((re) => re.test(url)), `${file}: unexpected URL ${url}`);
  }
  // The release version appears only as planned.
  for (const line of docs[JA].split("\n").filter((l) => l.includes("0.8.0"))) assert.match(line, /予定/);
  for (const line of docs[EN].split("\n").filter((l) => l.includes("0.8.0"))) assert.match(line, /planned/);
});

test("specification numbers agree with the code and between the languages", () => {
  const constant = (file, re) => Number(re.exec(read(file))[1].replace(/_/g, ""));
  const MiB = 1024 * 1024;
  const schema = constant("native-host/src/schema.rs", /pub const SCHEMA_VERSION: i64 = (\d+);/);
  const spell = constant("native-host/src/secure_vault/spell.rs", /pub const LEN: usize = (\d+);/);
  const port = constant("tray/src/settings.rs", /pub const DEFAULT_PORT: u16 = (\d+);/);
  const pixels = constant("extension/image-original.js", /export const MAX_DECODE_PIXELS = ([\d_]+);/);
  const imageMiB = constant("extension/image-format.js", /export const MAX_IMAGE_BYTES = (\d+) \* 1024 \* 1024;/);
  const pageMiB = constant("native-host/src/schema.rs", /byte_length BETWEEN 1 AND (\d+)\)/) / MiB;
  const chrome = Number(JSON.parse(read("extension/manifest.json")).minimum_chrome_version);
  const jaMd = docs[JA];
  const enMd = docs[EN];
  const same = (label, jaValues, enValues, expected) => {
    assert.ok(jaValues.length > 0 && enValues.length > 0, `${label} is stated in both`);
    for (const v of [...jaValues, ...enValues]) assert.ok(expected.includes(v), `${label}: ${v} is not ${expected.join(" or ")}`);
    assert.deepEqual([...enValues].sort(), [...jaValues].sort(), `${label} is stated the same in both`);
  };
  same("Recovery Spell length", all(jaMd, /(\d+)文字/g).map(Number), all(enMd, /(\d+) characters/g).map(Number), [spell]);
  same("size limits (MiB)", all(jaMd, /(\d+) ?MiB/g).map(Number), all(enMd, /(\d+) ?MiB/g).map(Number), [imageMiB, pageMiB]);
  assert.ok(all(jaMd, /(\d+) ?MiB/g).map(Number).includes(pageMiB) && all(jaMd, /(\d+) ?MiB/g).map(Number).includes(imageMiB));
  same("MCP port", all(jaMd, /127\.0\.0\.1:(\d+)/g).map(Number), all(enMd, /127\.0\.0\.1:(\d+)/g).map(Number), [port]);
  same("Chrome version", all(jaMd, /Chrome (\d+)以降/g).map(Number), all(enMd, /Chrome (\d+) or later/g).map(Number), [chrome]);
  same("image pixel limit", all(jaMd, /(\d+)万画素/g).map((n) => n * 1e4), all(enMd, /(\d+) megapixels/g).map((n) => n * 1e6), [pixels]);
  same("current schema", all(jaMd, /スキーマ(\d+)(?!\d|以前)/g).map(Number), all(enMd, /schema (\d+)(?!\d| or earlier)/g).map(Number), [schema]);
  same("refused schemas", all(jaMd, /スキーマ(\d+)以前/g).map(Number), all(enMd, /schema (\d+) or earlier/g).map(Number), [schema - 1]);
});

// Screen labels quoted in the READMEs, by message key: each must appear exactly as that language shows it.
const LABEL_KEYS = [
  "menuSaveImage", "menuSavePage", "panelRecordTabs", "panelOpenHistoryLabel", "panelPolicyInitialize", "panelPolicyRecheck",
  "panelPrivacySettings", "panelSettings", "optionsStartRecording", "vaultEncryptChoice", "vaultPlainChoice",
  "vaultCreateDefaultButton", "vaultCreatePickButton", "vaultOpenPickButton", "vaultSpellWrittenButton", "vaultRecoveryStartButton",
  "vaultConvertEncryptTitle", "vaultConvertDecryptTitle", "vaultReissueTitle", "vaultBackupPickButton", "vaultRestorePickButton",
  "vaultRestoreActivateButton", "vaultPublicationConfirmButton", "vaultPublicationApproveButton", "vaultPublicationRevokeButton",
  "calendarFilterByDate", "historyCalendarLabel", "historyDomainLabel", "historyMatchExact", "historyMatchRegex", "imageSourcesHeading",
  "imageTagsHeading", "imageMenuCopy", "imageMenuExport", "imageFilterByTag", "pageEventExport", "deleteTrashVisit", "deleteUrlAll",
  "deleteTrashImage", "deleteTrashPageSave", "deleteDialogTitle", "deleteConfirm", "rowChooseAction", "policyActionNeverSave",
  "policyActionSaveButNoMcp", "policyActionPublish", "policyLayerNeverSave", "policyLayerSaveButNoMcp", "errorHostUnavailable",
  "vaultStateNotConfigured",
];

test("quoted extension labels are the catalog's text in each language", () => {
  for (const key of LABEL_KEYS) {
    assert.ok(ja[key] && en[key], `${key} is in both catalogs`);
    assert.ok(docs[JA].includes(`「${ja[key].message}」`), `README.md quotes ${key}: 「${ja[key].message}」`);
    assert.ok(docs[EN].includes(`"${en[key].message}"`), `README.en.md quotes ${key}: "${en[key].message}"`);
    // The same label is quoted equally often, so neither language drops or renames one of its uses.
    const uses = (md, quoted) => md.split(quoted).length - 1;
    assert.equal(uses(docs[EN], `"${en[key].message}"`), uses(docs[JA], `「${ja[key].message}」`), `${key} is quoted equally often`);
  }
  assert.ok(docs[JA].includes("**ふっかつのじゅもん（Recovery Spell）**") && ja.vaultSpellTitle.message === "ふっかつのじゅもん（Recovery Spell）");
  assert.ok(docs[EN].includes("**Recovery Spell**") && en.vaultSpellTitle.message === "Recovery Spell");
  // A label quoted inside another message (the per-save MCP switch).
  assert.ok(ja.imagePublicationRule.message.includes("「MCPへの公開を許可」") && docs[JA].includes("「MCPへの公開を許可」"));
  assert.ok(en.imagePublicationRule.message.includes("“Allow publishing to MCP”") && docs[EN].includes('"Allow publishing to MCP"'));
});

test("quoted tray menu items are the tray's text in each language", () => {
  const tray = read("tray/src/i18n.rs");
  const items = [
    ["Windowsログオン時に起動", "Start with Windows"], ["MCPサーバを自動起動", "Start MCP server automatically"],
    ["サーバ起動", "Start server"], ["サーバ停止", "Stop server"], ["設定ファイルを開く", "Open configuration file"],
    ["ログフォルダを開く", "Open log folder"], ["終了", "Quit"],
  ];
  for (const [jaText, enText] of items) {
    assert.ok(tray.includes(`ja: "${jaText}"`) && tray.includes(`en: "${enText}"`), `tray has ${enText}`);
    assert.ok(docs[JA].includes(`「${jaText}」`), `README.md quotes 「${jaText}」`);
    assert.ok(docs[EN].includes(`"${enText}"`), `README.en.md quotes "${enText}"`);
  }
  assert.ok(tray.includes('ja: "Vault公開: 未確認"') && docs[JA].includes("「Vault公開」"));
  assert.ok(tray.includes('en: "Vault access: Not confirmed"') && docs[EN].includes('"Vault access"'));
});

// Safety statements: [section, Japanese pattern, English pattern]. Short key phrases within the section they
// belong to; presence only (see the note at the top).
const SAFETY = [
  ["the Windows app is needed", 2, /拡張機能だけでは[^。]*できません/, /cannot record on its own/],
  ["the installer is unsigned", 3, /コード署名がありません/, /not code-signed/],
  ["the Recovery Spell is shown once", 4, /一度しか表示/, /shown only once/],
  ["creating an encrypted backup needs the spell", 4, /バックアップ[^。]*作成[^。]*じゅもん/, /[Cc]reat[^.]*backup[^.]*spell/],
  ["pages are never offered to MCP", 5, /保存したページ[^。]*提供しません/, /[Ss]aved pages are not offered/],
  ["deletion is not physical erasure", 5, /物理的[^。]*保証しません/, /does not guarantee[^.]*physically/],
  ["backups keep deleted data", 5, /バックアップ[^。]*削除したデータ[^。]*残/, /[Bb]ackups[^.]*contain the deleted data/],
  ["Never Save does not delete past records", 6, /\| 「保存しないグループ」 \|[^\n]*削除しません/, /\| "Never Save group" \|[^\n]*does not delete/],
  ["Hide from MCP still records", 6, /\| 「MCPサーバに見せないグループ」 \|[^\n]*記録[^\n]*止めません/, /\| "Hide from MCP group" \|[^\n]*Recorded[^\n]*does not stop/],
  ["incognito images can be saved", 6, /シークレットウィンドウの画像[^。]*保存できます/, /[Ii]mages in incognito windows can be saved/],
  ["an unencrypted Vault is not encrypted on disk", 7, /\| ディスク上のデータ \|[^\n]*\|[^\n]*暗号化されません/, /\| Data on disk \|[^\n]*\|[^\n]*Not encrypted/],
  ["format A is not encrypted", 7, /\| A \|[^\n]*暗号化されません/, /\| A \|[^\n]*not encrypted/],
  ["format B needs the spell to create and to restore", 7, /\| B \|[^\n]*じゅもん[^\n]*じゅもん/, /\| B \|[^\n]*Recovery Spell[^\n]*Recovery Spell/],
  ["format D needs the password and the spell, both ways", 7, /\| D \|(?=[^\n]*じゅもん[^\n]*じゅもん)(?=[^\n]*パスワード[^\n]*パスワード)/, /\| D \|(?=[^\n]*Spell[^\n]*Spell)(?=[^\n]*Password[^\n]*Password)/],
  ["the password is stored nowhere", 7, /パスワードはどこにも保存しない/, /password is stored nowhere/],
  ["reissuing does not replace the old spell", 7, /古いじゅもん[^。]*置き換わ|置き換わるわけではありません/, /does not replace the old spell/],
  ["old backups still need the old spell", 7, /バックアップを復元するには、引き続き古いじゅもん/, /still needs the old spell/],
  ["MCP needs explicit approval", 8, /押した場合だけ/, /only after you press/],
  ["thumbnails, never the original", 8, /元の画像ファイルは渡しません/, /never the original image file/],
  ["image publication starts off", 8, /保存した直後はオフ/, /off right after saving/],
  ["data given to MCP clients leaves Memocampus", 8, /Memocampusの管理を離れます/, /leaves Memocampus's control/],
  ["removing the extension loses unsent records", 9, /届いていない記録[^。]*失われます/, /loses the records that have not reached the Vault/],
];

test("the safety statements users rely on are present in both languages, in their sections", () => {
  for (const [name, n, jaPattern, enPattern] of SAFETY) {
    assert.match(section(docs[JA], n), jaPattern, `README.md section ${n}: ${name}`);
    assert.match(section(docs[EN], n), enPattern, `README.en.md section ${n}: ${name}`);
  }
  // Format C (an unencrypted Vault with a password) never needs the spell.
  assert.doesNotMatch(section(docs[JA], 7), /\| C \|[^\n]*じゅもん/);
  assert.doesNotMatch(section(docs[EN], 7), /\| C \|[^\n]*Spell/);
  // No unconditional claim that nothing ever leaves the PC. Known wordings only (a keyword list, not a
  // reading of the sentence): "never leave(s)", "never sent/sends", "nothing (ever) leaves".
  assert.doesNotMatch(docs[JA], /外部(に|へ)?(一切)?送信しません/);
  assert.doesNotMatch(docs[EN], /never (leaves?|sent|sends)|nothing (ever )?leaves/i);
});
