#!/usr/bin/env node
// Lists the DLLs a Windows PE file imports (normal + delay-load), read straight from the file.
// No tools, no network. Used to prove the release executables do not need the Visual C++ runtime.
//
//   node scripts/pe-imports.mjs <exe> [<exe> ...]            prints the imports
//   node scripts/pe-imports.mjs --check <exe> [<exe> ...]    exit 1 if any VC++ runtime DLL is imported
//
// Exports: peImports(buffer) -> { imports: string[], delayImports: string[] }, forbiddenRuntime(names)
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

// Redistributable Visual C++ / UCRT-redist DLLs that a clean Windows does not necessarily have.
// (api-ms-win-crt-* and ucrtbase.dll are the Universal CRT, part of Windows 10/11 itself: allowed.)
// A name is a runtime family, its version digits, and any further `_suffix` parts (vcruntime140_1,
// vcruntime140_threads, msvcp140_atomic_wait, msvcp140_codecvt_ids, ...). msvcrt.dll has no digits: it is Windows's own.
const FORBIDDEN = /^(vcruntime|msvcp|concrt|vcomp|vccorlib|vcamp|msvcr)\d+(_[a-z0-9]+)*\.dll$|^mfc\d+u?(_[a-z0-9]+)*\.dll$/i;

export function forbiddenRuntime(names) { return names.filter((n) => FORBIDDEN.test(n)); }

export function peImports(buf) {
  if (buf.length < 0x40 || buf.readUInt16LE(0) !== 0x5a4d) throw new Error('not a PE file (no MZ)');
  const pe = buf.readUInt32LE(0x3c);
  if (buf.readUInt32LE(pe) !== 0x4550) throw new Error('not a PE file (no PE signature)');
  const nSec = buf.readUInt16LE(pe + 6);
  const optSize = buf.readUInt16LE(pe + 20);
  const opt = pe + 24;
  const magic = buf.readUInt16LE(opt);
  const plus = magic === 0x20b;
  if (!plus && magic !== 0x10b) throw new Error('unknown optional header');
  const dirBase = opt + (plus ? 112 : 96);
  const secs = [];
  for (let i = 0; i < nSec; i++) {
    const s = opt + optSize + i * 40;
    secs.push({ va: buf.readUInt32LE(s + 12), vsize: buf.readUInt32LE(s + 8), raw: buf.readUInt32LE(s + 20), rawSize: buf.readUInt32LE(s + 16) });
  }
  const rva = (r) => {
    for (const s of secs) if (r >= s.va && r < s.va + Math.max(s.vsize, s.rawSize)) return r - s.va + s.raw;
    throw new Error(`RVA 0x${r.toString(16)} is in no section`);
  };
  const str = (off) => {
    if (off < 0 || off >= buf.length) throw new Error('malformed PE: a name starts outside the file');
    let e = off;
    while (e < buf.length && buf[e] !== 0) e++;
    if (e >= buf.length) throw new Error('malformed PE: a name has no terminating NUL before the end of the file');
    return buf.toString('latin1', off, e);
  };
  const out = { imports: [], delayImports: [] };
  const imp = buf.readUInt32LE(dirBase + 8);
  if (imp) {
    for (let o = rva(imp); ; o += 20) {
      const name = buf.readUInt32LE(o + 12);
      if (!name && !buf.readUInt32LE(o) && !buf.readUInt32LE(o + 16)) break;
      out.imports.push(str(rva(name)));
    }
  }
  const delay = buf.readUInt32LE(dirBase + 13 * 8);
  if (delay) {
    for (let o = rva(delay); ; o += 32) {
      const name = buf.readUInt32LE(o + 4);
      if (!name) break;
      out.delayImports.push(str(rva(name)));
    }
  }
  return out;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const args = process.argv.slice(2);
  const check = args[0] === '--check';
  const files = check ? args.slice(1) : args;
  if (!files.length) { console.error('usage: pe-imports.mjs [--check] <exe>...'); process.exit(2); }
  let bad = 0;
  for (const f of files) {
    const r = peImports(readFileSync(f));
    const all = [...r.imports, ...r.delayImports];
    const forb = forbiddenRuntime(all);
    console.log(`${f}`);
    console.log(`  imports (${r.imports.length}): ${[...new Set(r.imports.map((x) => x.toLowerCase()))].sort().join(', ')}`);
    if (r.delayImports.length) console.log(`  delay-load: ${[...new Set(r.delayImports.map((x) => x.toLowerCase()))].sort().join(', ')}`);
    if (forb.length) { console.log(`  VC++ RUNTIME DEPENDENCY: ${forb.join(', ')}`); bad++; }
    else console.log('  no Visual C++ runtime DLL');
  }
  if (check && bad) { console.error(`${bad} executable(s) need the Visual C++ runtime`); process.exit(1); }
}
