// scripts/pe-imports.mjs: reads the import table of a PE file and flags Visual C++ runtime DLLs.
// A tiny PE32+ file is built here by hand (one section, an import directory), so the test needs no tool.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { peImports, forbiddenRuntime } from '../../scripts/pe-imports.mjs';

function makePe(dlls) {
  const buf = Buffer.alloc(0x1000);
  buf.write('MZ', 0, 'latin1');
  buf.writeUInt32LE(0x80, 0x3c);
  const pe = 0x80;
  buf.write('PE\0\0', pe, 'latin1');
  buf.writeUInt16LE(1, pe + 6);            // one section
  buf.writeUInt16LE(240, pe + 20);         // optional header size (PE32+)
  const opt = pe + 24;
  buf.writeUInt16LE(0x20b, opt);
  const dir = opt + 112;
  const secHdr = opt + 240;
  const va = 0x1000, raw = 0x400;
  buf.writeUInt32LE(0x1000, secHdr + 8);   // virtual size
  buf.writeUInt32LE(va, secHdr + 12);
  buf.writeUInt32LE(0x600, secHdr + 16);   // raw size
  buf.writeUInt32LE(raw, secHdr + 20);
  const big = Buffer.alloc(raw + 0x600); buf.copy(big, 0, 0, 0x400);
  // import descriptors at section offset 0, names after them
  const descBytes = (dlls.length + 1) * 20;
  let namePos = descBytes;
  dlls.forEach((d, i) => {
    big.writeUInt32LE(va + namePos, raw + i * 20 + 12);
    big.writeUInt32LE(va + 0x500, raw + i * 20);      // OriginalFirstThunk (non-zero)
    big.write(d + '\0', raw + namePos, 'latin1');
    namePos += d.length + 1;
  });
  big.writeUInt32LE(va, opt + 112 + 8);                // import directory RVA
  big.writeUInt32LE(descBytes, opt + 112 + 12);
  return big;
}

test('reads the imported DLL names', () => {
  const r = peImports(makePe(['KERNEL32.dll', 'api-ms-win-crt-runtime-l1-1-0.dll']));
  assert.deepEqual(r.imports, ['KERNEL32.dll', 'api-ms-win-crt-runtime-l1-1-0.dll']);
});

test('flags the Visual C++ runtime DLLs and nothing else', () => {
  const bad = ['VCRUNTIME140.dll', 'vcruntime140_1.dll', 'MSVCP140.dll', 'msvcp140_1.dll', 'CONCRT140.dll', 'VCOMP140.dll', 'MSVCR120.dll'];
  assert.deepEqual(forbiddenRuntime(bad), bad);
  const ok = ['KERNEL32.dll', 'ucrtbase.dll', 'api-ms-win-crt-heap-l1-1-0.dll', 'USER32.dll', 'bcryptprimitives.dll', 'WS2_32.dll'];
  assert.deepEqual(forbiddenRuntime(ok), []);
});

test('a file that is not a PE is refused', () => {
  assert.throws(() => peImports(Buffer.from('hello world, not an executable at all........................................')));
});

test('an import table that names VCRUNTIME140.dll is found end to end', () => {
  const r = peImports(makePe(['KERNEL32.dll', 'VCRUNTIME140.dll']));
  assert.deepEqual(forbiddenRuntime(r.imports), ['VCRUNTIME140.dll']);
});

test('the newer runtime DLL names are flagged too (suffixed vcruntime/msvcp)', () => {
  const bad = ['msvcp140_atomic_wait.dll', 'vcruntime140_threads.dll', 'msvcp140_codecvt_ids.dll', 'MSVCP140_2.dll', 'vcruntime140_1.dll', 'VCRUNTIME140.dll', 'MFC140U.dll', 'mfc140u_x.dll'];
  assert.deepEqual(forbiddenRuntime(bad), bad);
});

test('Windows own DLLs are not mistaken for the runtime', () => {
  const ok = ['msvcrt.dll', 'ucrtbase.dll', 'KERNEL32.dll', 'vcruntime.txt', 'xvcruntime140.dll', 'vcruntime140.dll.manifest', 'comctl32.dll', 'api-ms-win-crt-runtime-l1-1-0.dll', 'dbghelp.dll', 'msvcp_win.dll', 'bcryptprimitives.dll'];
  assert.deepEqual(forbiddenRuntime(ok), []);
});

test('a name with no terminating NUL before the end of the file is an error, not a cut-off name', () => {
  const full = makePe(['KERNEL32.dll', 'VCRUNTIME140.dll']);
  const at = full.indexOf('VCRUNTIME140.dll');
  const cut = full.subarray(0, at + 'VCRUNTIME140.dll'.length); // the NUL and everything after are gone
  assert.throws(() => peImports(cut), /no terminating NUL/);
  const cutMid = full.subarray(0, at + 7);
  assert.throws(() => peImports(cutMid), /no terminating NUL/);
});

test('a name pointer outside the file is an error', () => {
  const buf = makePe(['KERNEL32.dll']);
  const full = Buffer.from(buf);
  const sec = full.readUInt32LE(0x80 + 24 + 240 + 20);
  full.writeUInt32LE(0x1000 + 0x7000, sec + 12); // name RVA far outside
  assert.throws(() => peImports(full), /malformed PE|in no section/);
});
