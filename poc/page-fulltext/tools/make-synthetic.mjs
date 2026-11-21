// Writes SYNTHETIC large MHTML files shaped like Chrome's output, for size/performance runs only.
//
//   node tools/make-synthetic.mjs --out <dir>
//
// Nothing here comes from a real page. An existing file is never overwritten.
import { writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { randomBytes } from "node:crypto";

const out = process.argv[process.argv.indexOf("--out") + 1];
if (!out || out === "--out") { console.error("usage: make-synthetic.mjs --out <dir>"); process.exit(2); }

const B = "----MultipartBoundary--SYNTHETICxPERFxRUNxxxxxxxxxxxxxxxx----";
function qp(buf) {
  let out = "", line = 0;
  for (const b of buf) {
    let e;
    if (b === 0x0a) { out += "\r\n"; line = 0; continue; }
    e = (b === 0x3d || b >= 0x7f || (b < 0x20 && b !== 0x09)) ? "=" + b.toString(16).toUpperCase().padStart(2, "0") : String.fromCharCode(b);
    if (line + e.length > 75) { out += "=\r\n"; line = 0; }
    out += e; line += e.length;
  }
  return out;
}
function b64lines(buf) {
  const s = buf.toString("base64");
  const lines = [];
  for (let i = 0; i < s.length; i += 76) lines.push(s.slice(i, i + 76));
  return lines.join("\r\n");
}
function mhtml(html, resources) {
  const chunks = [
    `From: <Saved by Blink>\r\nSnapshot-Content-Location: https://synthetic.test/perf\r\nSubject: synthetic\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related;\r\n\ttype="text/html";\r\n\tboundary="${B}"\r\n\r\n\r\n`,
    `--${B}\r\nContent-Type: text/html\r\nContent-ID: <frame-ROOT@mhtml.blink>\r\nContent-Transfer-Encoding: quoted-printable\r\nContent-Location: https://synthetic.test/perf\r\n\r\n${qp(Buffer.from(html, "utf8"))}\r\n`,
  ];
  resources.forEach((r, i) => chunks.push(`--${B}\r\nContent-Type: image/png\r\nContent-Transfer-Encoding: base64\r\nContent-Location: https://synthetic.test/img${i}.png\r\n\r\n${b64lines(r)}\r\n`));
  chunks.push(`--${B}--\r\n`);
  return Buffer.from(chunks.join(""), "latin1");
}
const head = '<!DOCTYPE html><html lang="ja"><head><meta http-equiv="Content-Type" content="text/html; charset=UTF-8"><title>合成性能ページ</title></head><body>';
const para = (i) => `<p>${i}: 合成の段落です。Synthetic paragraph with mixed 日本語 and English for measurement.</p>\n`;
function textPage(paragraphs) { let s = head; for (let i = 0; i < paragraphs; i++) s += para(i); return s + "</body></html>"; }
function markupPage(targetBytes) {
  let s = head, i = 0;
  while (s.length < targetBytes) { s += `<div class="c${i % 7}"><span><a href="#a${i}">項目${i}</a></span><span class="x"> </span><i></i><b>b</b></div>\n`; i++; }
  return s + "</body></html>";
}
const files = {
  // ~100 MB: 1 MB of HTML text and resources (random bytes, base64) making up the rest.
  "synthetic-100mb-resources.mhtml": () => mhtml(textPage(9000), Array.from({ length: 24 }, () => randomBytes(3 * 1024 * 1024))),
  // Below the 128 MiB input limit (~125 MiB).
  "synthetic-125mib-resources.mhtml": () => mhtml(textPage(9000), Array.from({ length: 30 }, () => randomBytes(3 * 1024 * 1024))),
  // Above the limit (~129 MiB): must be refused before reading.
  "synthetic-over-limit-resources.mhtml": () => mhtml(textPage(9000), Array.from({ length: 31 }, () => randomBytes(3 * 1024 * 1024))),
  // HTML part close to the 16 MiB decoded limit, markup-heavy (large DOM, moderate text).
  "synthetic-15mib-html-markup.mhtml": () => mhtml(markupPage(15 * 1024 * 1024), []),
  // Added for the tokenizer comparison (report XIII): inputs that stress the extractor's state.
  // 1.4 million nested <div>s (~15 MB of HTML).
  "synthetic-deep-nesting.mhtml": () => mhtml(head + "<div>".repeat(1_400_000) + "深い本文" + "</div>".repeat(1_400_000) + "</body></html>", []),
  // A deep stack of <span>s followed by 3.5 million end tags that match nothing (~14 MB).
  "synthetic-stray-end-tags.mhtml": () => mhtml(head + "<span>".repeat(1000) + "</x>".repeat(3_500_000) + "本文</body></html>", []),
  // One 15 MiB comment between two paragraphs.
  "synthetic-huge-comment.mhtml": () => mhtml(head + "<p>前</p><!--" + "c".repeat(15 * 1024 * 1024) + "--><p>後</p></body></html>", []),
  // One 15 MiB attribute value (an inline data: URL).
  "synthetic-huge-attribute.mhtml": () => mhtml(head + '<p>前</p><img src="data:image/png;base64,' + "A".repeat(15 * 1024 * 1024) + '"><p>後</p></body></html>', []),
};
for (const [name, make] of Object.entries(files)) {
  const path = join(out, name);
  if (existsSync(path)) { console.log(`${name}: exists, kept`); continue; }
  const bytes = make();
  writeFileSync(path, bytes, { flag: "wx" });
  console.log(`${name}: ${bytes.length} bytes`);
}
