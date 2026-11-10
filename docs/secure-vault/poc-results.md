# Secure Vault — PoC results (Issue #51, Slice A)

> Note: the A-3 measurements below concern the 12-word BIP-39 recovery phrase, which the build no longer uses; it was
> replaced by the Recovery Spell (`recovery-spell.md`). They are kept as the record of the PoC.

## Environment

| Item | Value |
|---|---|
| Basis | `main` @ `c08797a`; PoC on branch `design/51-secure-vault`, uncommitted |
| OS | Windows 11 Home 10.0.26220, x86_64 |
| Toolchain | rustc 1.98.1 (`x86_64-pc-windows-msvc`), cargo 1.98.1 |
| PoC deps | rusqlite 0.32.1 / libsqlite3-sys 0.30.1 (`bundled`, SQLite 3.46.0), bip39 2.2.2, chacha20poly1305 0.10.1, hkdf 0.12.4, hmac 0.12.1, sha2 0.10.9, argon2 0.5.3, windows-sys 0.61.2 |
| Production crates | used read-only as path dependencies (`schema::check`, `schema::create_sql`, `VisibleStore`); **not modified** |
| Data | synthetic only, in fresh `%TEMP%\mc-sv-poc-*` directories that are removed afterwards; no real vault, policy, DPAPI store entry or Chrome profile was touched |

Code: `poc/secure-vault/crypto-poc` (library `src/lib.rs`, tests `tests/poc.rs`),
`poc/secure-vault/cipher-poc` (SQLCipher, `tests/cipher.rs`), `poc/secure-vault/size-plain`,
`poc/secure-vault/size-cipher`.

SQLCipher builds (A'):

- MSYS2 `mingw-w64-x86_64-perl` 5.44.0 (MSWin32-x64, installed by the user with pacman), passed as
  `PERL`, first on `PATH`, with `LANG` unset.
- Production crates were tested as **copies** in the scratch directory, with only their `rusqlite`
  feature switched to `bundled-sqlcipher-vendored-openssl`. The repository files were not changed.

Command: `cargo test --release --target-dir <scratch> -- --nocapture --test-threads=1`.

Results:

- crypto-poc: **14 passed, 0 failed**;
- cipher-poc: **7 passed, 0 failed** (1 ignored, the crash child);
- production suites on SQLCipher: **all passed** (A-1.5).

## Summary

| ID | PoC | Result |
|---|---|---|
| A-1.1 | Plain baseline build (size and time) | **PASS** (measured) |
| A-1.2 | `bundled-sqlcipher-vendored-openssl` build on MSVC | **PASS** (needs a native Perl) |
| A-1.3 | Plain vault from a plain build, opened by the SQLCipher build without a key | **PASS**; one binary cannot hold both (source analysis) |
| A-1.4 | Binary size, build time and licence of SQLCipher | **PASS** (measured); licence [DOC] |
| A-1.5 | Production test suites and `schema::check` on SQLCipher | **PASS** (native-host 430, visible-store 102 + 2 ignored, mcp-server 52) |
| A-1.6 | Encrypted vault: header, journal, salt | **PASS** |
| A-2.1 | CSPRNG vault keys (`BCryptGenRandom`) | **PASS** |
| A-2.2 | DPAPI user-scope protect/unprotect; wrong entropy, corruption, truncation | **PASS with finding** (blob partly malleable, never a different key) |
| A-2.3 | Phrase unwrap, then re-protect with DPAPI ("new PC") | **PASS** on the same machine and user; a real second PC is **NOT RUN** |
| A-2.4 | Unlock by another Windows user fails | **NOT RUN** (needs a second account) |
| A-3.1 | BIP-39 official vectors, wordlist properties | **PASS** |
| A-3.2 | Typos and swaps never unlock | **PASS** |
| A-3.3 | Envelope binding; phrase regeneration vs old backups | **PASS** |
| A-4.1 | Plain to plain: Backup API and `VACUUM INTO` | **PASS** |
| A-4.2 | Plain: raw byte copy under SHARED is a consistent snapshot | **PASS** |
| A-4.3 | SQLCipher matrix: Backup API / `sqlcipher_export` / `VACUUM INTO` | **PASS with finding** (`sqlcipher_export` drops `application_id`/`user_version`) |
| A-4.4 | Raw ciphertext copy under SHARED | **PASS** |
| A-4.5 | Crash (process abort) during `sqlcipher_export` | **PASS** (source intact; the partial target is *openable*, so verification before publication is mandatory) |
| A-5.1 | Format B container round trip and fail-closed | **PASS** (payload is a stand-in, not SQLCipher) |
| A-5.2 | Format C over A: restore into a new vault only; fail-closed | **PASS** |
| A-5.3 | Format C over B: needs password AND phrase | **PASS** |
| A-5.4 | Argon2id cost | **PASS** (measured) |
| A-7.1 | Key-independent exclusive-handle fence | **PASS** |
| A-8.1 | Encrypted-looking vault under a plain build and VisibleStore (HIGH-4/HIGH-5 mechanism) | **PASS** (reproduced) |
| A-8.2 | Real SQLCipher vault with no key / wrong key | **PASS** (reproduced: `NotADatabase`/26 for both read and `BEGIN IMMEDIATE`) |

---

## A-1 SQLCipher build

**A-1.1 Baseline.**

- Command: `poc/secure-vault/size-plain` (`rusqlite =0.32.1`, `bundled`, `backup`, the same features
  as production), `cargo build --release`.
- Result:
  - 28 s wall time (cold target dir, warm crate cache);
  - `size-plain.exe` 1,747,968 bytes;
  - reports `sqlite_version=3.46.0`, no `cipher_version`.

**A-1.2 SQLCipher with vendored OpenSSL.**

- Command: `poc/secure-vault/size-cipher` (`bundled-sqlcipher-vendored-openssl`).
- First attempt failed:
  - Git-for-Windows' MSYS Perl lacks `Locale::Maketext::Simple`, and OpenSSL's MSVC build requires a
    native Perl.
  - A portable Strawberry Perl was downloaded and its digest checked, but running it was refused by
    the session's permission policy.
- With MSYS2 `mingw-w64-x86_64-perl` (native MSWin32) the build **succeeded**:
  - 11 min 48 s cold release build (OpenSSL 3.6.3 via `openssl-src 300.6.1`, `no-asm`, serial
    `nmake`);
  - `size-cipher.exe` 4,722,176 bytes, against 1,747,968 for plain (+2.97 MB);
  - reports `sqlite_version=3.45.3` and `cipher_version=4.5.7 community`.
- Production binaries built from the switched copies:

  | Binary | SQLCipher | Plain | Difference |
  |---|---|---|---|
  | `memocampus-native-host.exe` | 8,365,056 | 5,390,848 | +2.97 MB |
  | `memocampus-mcp.exe` | 12,088,320 | 9,112,576 | +2.98 MB |

  The plain reference binaries were built earlier from the #48 branch, whose code is the same as
  `main` for this purpose.
- Build settings:
  - `PRAGMA compile_options` includes `HAS_CODEC`, `TEMP_STORE=2` and `THREADSAFE=1`.
  - `PRAGMA temp_store` reads 0 (default), so temporary storage is memory unless a connection
    changes it.
- Keyed defaults:
  - page size 4096, HMAC-SHA512;
  - PBKDF2-HMAC-SHA512 at 256,000 iterations (unused with a raw key);
  - `cipher_plaintext_header_size=0`, `journal_mode=delete`.
  - `PRAGMA cipher_provider` returned no row in this build.
- CI implication:
  - every build machine needs a native Perl (MSYS2 mingw64 or Strawberry), or a prebuilt MSVC OpenSSL
    through `OPENSSL_DIR`;
  - the cold build adds about 12 minutes, so cache the target directory.

**A-1.3 Coexistence (source analysis, [DOC]).**

- `libsqlite3-sys` 0.30.1: `bundled-sqlcipher = ["bundled"]`, and the build compiles
  `sqlcipher/sqlite3.c` instead of `sqlite3/sqlite3.c` when the feature is on.
- The crate declares `links = "sqlite3"`, so a binary has exactly one SQLite.
- Feature unification would turn every crate in a graph to SQLCipher, so native-host, visible-store
  and mcp-server (three workspaces, three binaries) must switch together.
- Bundled versions: **SQLCipher 4.5.7 on SQLite 3.45.3** (`sqlcipher/sqlite3.c:499,106612`), against
  3.46.0 for plain `bundled`. Switching is a SQLite downgrade.
- Flags set by the crate: `SQLITE_HAS_CODEC` and `SQLITE_TEMP_STORE=2` (`build.rs:144`).

**A-1.4 Licence [DOC].**

- SQLCipher: BSD-3-style, `Copyright (c) 2008-2020 Zetetic LLC` (`sqlcipher/LICENSE`); binaries must
  reproduce the notice.
- OpenSSL 3.x: Apache-2.0.
- Both must go into `THIRD_PARTY_NOTICES.md` and `LICENSES/`. This is not legal advice.

**A-1.3 / A-1.5 Regression on SQLCipher (measured).**

- A schema 7 vault written by the **plain** build (crypto-poc fixture, 300 visits) opens in the
  SQLCipher build without a key: `quick_check` ok, `schema::check` ok, ids `(0x4D434D50, 7)`. After a
  write it is still a plain SQLite file.
- Production suites on SQLCipher copies of the crates (release profile):

  | Crate | Result |
  |---|---|
  | native-host | 99 + 226 + 1 + 3 + 2 + 1 + 90 + 4 + 4 = **430 passed** (the same total as on plain SQLite) |
  | visible-store | 94 + 1 + 7 = **102 passed**, 2 ignored (`#[ignore]` in the source) |
  | mcp-server | 20 + 13 + 19 = **52 passed** |

- The SQLite 3.46.0 → 3.45.3 downgrade broke no existing test.
- A full Node/E2E run against a SQLCipher host binary was **NOT RUN**.

**A-1.6 Encrypted vault (C-3).**

- A 3,000-visit vault was created with the production `create_sql` under a raw key.
  - It passes `schema::check`, and its ids are kept.
  - The file does not start with the SQLite magic, and the marker URL is absent from it.
- The rollback journal, written while a transaction spilled (318,984 bytes), does not contain the
  marker: journal pages are encrypted.
- The first 16 bytes (the salt) stay the same across writes.

## A-2 Key management

**A-2.1 CSPRNG.**

- `BCryptGenRandom(NULL, …, BCRYPT_USE_SYSTEM_PREFERRED_RNG)`.
- 10,000 keys of 32 bytes: all distinct, none zero; bit ratio 0.4998–0.5004 over three runs.
- This is a sanity check, not a statistical RNG test.

**A-2.2 DPAPI** (`CryptProtectData`/`CryptUnprotectData`, user scope, `CRYPTPROTECT_UI_FORBIDDEN`,
entropy = purpose label and vault id).

| Case | Result |
|---|---|
| Round trip | ok; blob 262 bytes; protect 1.1–1.2 ms, unprotect 0.15–0.19 ms; VK not in the blob in clear |
| Wrong entropy | refused, error 13 (`ERROR_INVALID_DATA`) |
| Truncated by one byte | refused, 13 |
| Empty blob | refused, 87 (`ERROR_INVALID_PARAMETER`) |
| Exhaustive single-bit flips (2,096) | **1,968 refused** (errors 13, `0x8009000B` NTE_BAD_KEY_STATE, `0xD000000D` STATUS_INVALID_PARAMETER_*); **128 accepted, all yielding the same VK**; **0 yielded different data** |

Finding:

- DPAPI does not authenticate some header bytes of its blob, but it never decrypted to a different
  key.
- The design adds its own key-check value (kcv) and binds the vault id in the entropy, so a
  substituted or altered key-store entry is detected by Memocampus itself.

**A-2.3 New PC.**

- Phrase entered in upper case with extra spaces, then parsed, then the envelope unwrapped, giving
  VK.
- `CryptProtectData` produced a new blob, different from the old one, and it unprotected to VK.
- Only the same machine and user was used. The "new PC" is modelled by discarding the old blob.
  A real second PC or OS reinstall is **NOT RUN**.

**A-2.4 Another user: NOT RUN.** It needs a second Windows account; none was created, by policy.
Expected per Microsoft documentation [DOC]: another user's logon credentials cannot decrypt a
user-scope blob.

## A-3 Recovery phrase

**A-3.1** The four official BIP-39 vectors for 128-bit entropy match in both directions:

- `00…00` gives `abandon ×11 about`;
- `7f…7f` gives `legal winner … yellow`;
- `80…80` gives `letter advice … above`;
- `ff…ff` gives `zoo ×11 wrong`.

The wordlist has 2,048 words, and all 4-letter prefixes are unique.

**A-3.2** The checksum alone is not a sufficient check; the AEAD step catches what it misses.

| Input change | Tried | Passed the checksum | Unlocked |
|---|---|---|---|
| Every single-word substitution | 24,564 | 1,536–1,544 (6.25–6.29%, as expected for 4 bits) | **0** |
| Adjacent swaps | 11 | 0 in one run, 2 in another | **0** |
| 11 words | refused `WordCount(11)` | — | — |
| Unknown word at position 5 | refused `UnknownWord(5)` | — | — |

**A-3.3**

- Wrong vault id, a changed salt or an unknown scheme are refused (`Rejected` or `Unsupported`).
- Regeneration: the same VK under a new phrase.
  - The new phrase opens the new envelope.
  - The old phrase still opens old envelopes, as in old backups.
  - The new phrase does not open old envelopes, and the old phrase does not open the new one.
- So regeneration is not revocation (HIGH-2).

## A-4 SQLite backup paths

**A-4.1 Plain to plain.**

- Source: a production `create_sql` vault with 1,000 visits and one image BLOB.
- Backup API inside a read transaction, as in production `copy_and_verify`. A second connection's
  commit during the snapshot got `DatabaseBusy`.
- Result: `integrity_check` ok, production `schema::check` ok, `application_id`/`user_version` kept,
  per-table content digests equal.
- `VACUUM INTO`: the same checks, all ok.
- No `-journal` or `-wal` files left.

**A-4.2 Raw copy under SHARED** (basis for format B and C creation without plaintext temp files).

- 2,000-visit vault. `BEGIN` plus a read took SHARED. A writer's
  `BEGIN IMMEDIATE; INSERT; COMMIT` failed with `DatabaseBusy`.
- `fs::read` of the file succeeded while SQLite held it open.
- The copy passes `integrity_check` and `schema::check`, and its content equals what the reader saw.
- Not covered: vaults larger than 1 GiB, which cross the lock-byte page. On Windows, reading that
  range under a lock fails; design §5.3 skips it. Not run.

**A-4.3 SQLCipher matrix (C-5, measured).**

The same 2,000-visit synthetic data was put in a plain source and an encrypted source (KEY_A). Every
output was opened with each candidate key, then verified.

| Path | Result |
|---|---|
| Backup API plain → plain | ok: integrity, `schema::check`, ids kept, content equal |
| Backup API cipher → cipher, dest KEY_A | ok, all checks; encrypted |
| Backup API cipher → cipher, dest KEY_B | ok, all checks; **re-encrypted under KEY_B** |
| Backup API plain → cipher | **refused**: `backup is not supported with encrypted databases` |
| Backup API cipher → plain | **refused**: same message |
| `sqlcipher_export` plain → cipher / cipher → plain / cipher → cipher (new key) | content equal, integrity ok, but **`application_id` = 0 and `user_version` = 0, so `schema::check` fails with `UnsupportedVersion(0)`** |
| `sqlcipher_export`, then `PRAGMA t.application_id`/`t.user_version` set from the source | ok, all checks, all three directions |
| `VACUUM INTO` plain | ok, all checks; plain |
| `VACUUM INTO` cipher (KEY_A) | ok, all checks; the output is **encrypted with the same key** |

Consequences for the design:

- Cross-mode conversion must use `sqlcipher_export` **plus explicit ids**, followed by
  `schema::check`.
- `PRAGMA user_version` on the result is not optional.

**A-4.4 Raw ciphertext copy under SHARED (C-6).**

- A writer's commit to the 3,000-visit encrypted vault during the copy failed with `DatabaseBusy`.
- The copy (430,080 bytes) has the same salt and opens with KEY_A only.
- `integrity_check` ok, `schema::check` ok, ids kept, content equal, marker absent.
- This is the format B method. A vault over 1 GiB (lock-byte page) was **NOT RUN**.

**A-4.5 Crash during export (C-7).**

- A child process ran an encrypted → plain `sqlcipher_export` into `target.partial`, and called
  `abort()` from a progress handler (exit code `0xC0000409`).
- Source: bytes unchanged; integrity, schema and content ok.
- Target:
  - `target.partial` (98,304 bytes) and `target.partial-journal` were left behind.
  - No plaintext marker was found in the file at that moment.
  - Reopened, SQLite rolled the hot journal back and showed **21 objects: the full schema with
    partial rows**, an openable but incomplete database.
- So a leftover `.partial` must never be activated or published without full verification (counts
  and digests against the source).
- For encrypted → plain conversion the partial is plaintext (R6 class).

## A-5 Backup container

**A-5.1 Format B.**

- 300,000-byte stand-in payload; container overhead 614 bytes.
- The policy copy marker is absent from the file bytes (sealed under a VK-derived key).
- Round trip: VK, vault id, payload and policy all recovered.

| Case | Result |
|---|---|
| Wrong phrase | `PhraseRejected` |
| Truncated by 1 / extended by 1 | `Incomplete` |
| Version 2 | `UnsupportedVersion` |
| `header_len` = u32::MAX | `Malformed` (no allocation) |
| Every 97th byte flipped (3,100 positions) | 3,094 `Tampered`, 5 `Malformed`, 1 `UnsupportedFormat`; **0 accepted** |

**A-5.2 Format C over A.**

- Source: a 500-visit production-schema vault, 155,648 bytes.
- Container 156,134 bytes. Neither the marker URL, the policy marker nor the `SQLite format 3` text
  appears in it.
- Restore: streamed into a `create_new` `.partial` in a separate folder.
  - `integrity_check` ok, `schema::check` ok, ids equal, content equal.
  - Renamed to a new name; only that file remains.
  - The policy copy was returned to the caller, not written anywhere.
- Fail-closed cases:

  | Case | Result |
  |---|---|
  | Wrong password | `PasswordRejected` |
  | Truncated | `Incomplete` |
  | Flip in the last chunk | `Tampered` |
  | First two chunks swapped | `Tampered` |
  | Header byte flipped | error |
  | `mKiB` = u32::MAX | `Malformed` in under 50 ms |

- Finding, fixed during the PoC: before adding the password check value, a tampered first chunk was
  reported as `PasswordRejected`.

**A-5.3 Format C over B.**

- The phrase alone, applied to the C file, gives `UnsupportedFormat`.
- The password alone gives the inner B package, which still needs the phrase; with a wrong phrase,
  `PhraseRejected`.
- Password and phrase together give VK and the payload.

**A-5.4 Argon2id** (m = 64 MiB, t = 3, p = 1): 77–81 ms on this machine. On a slow PC it may take
several times longer; the parameters stay bounded at restore time.

Not covered:

- containers over 4 GiB;
- a real SQLCipher payload;
- streaming restore of a large vault to a slow disk;
- crash during publication (beyond the code path).

## A-7 Fences and conversion

**A-7.1 Exclusive-handle fence.**

- While a rusqlite connection has the vault open, `OpenOptions::share_mode(0)` fails with OS error
  32 (sharing violation).
- With the fence held, a new SQLite connection's first read fails with `CannotOpen`, not
  `NotADatabase`, so today's code would classify it as `Io` (temporary).
- After release, everything works.
- So a switch can be fenced without the vault key.

Conversion algorithm, `maintenance` protocol and watchdog: design only (architecture §8),
**NOT RUN**.

## A-8 Error classification

**A-8.1** A 32 KiB random file stands in for SQLCipher ciphertext as seen without the key.

- Plain SQLite: `PRAGMA application_id` fails with `NotADatabase` (extended 26), and the file is
  unchanged.
- Today `OpenFailure::from_sqlite` maps this to `Invalid` with `Sqlite{NotADatabase}`, and
  `proves_unwritable()` returns **true** (`vault.rs:282,302-325`), so `switch_active` would skip the
  fence (`vault.rs:452`).
- This confirms the HIGH-4 mechanism at code level. A key-less open of a real encrypted vault takes
  the same branch; this was measured on SQLCipher in A-8.2.
- Production `VisibleStore::check_vault` reports `vault_schema_mismatch` on the same file, which
  reproduces HIGH-5: an encrypted vault would be misreported, failing closed but with the wrong
  reason.

**A-8.2 Real SQLCipher vault (C-4, measured).**

| Open | Read | `BEGIN IMMEDIATE` | `schema::check` |
|---|---|---|---|
| No key | `NotADatabase`/26 | `NotADatabase`/26 | `Sqlite(NotADatabase)` |
| Wrong key | `NotADatabase`/26 | `NotADatabase`/26 | `Sqlite(NotADatabase)` |
| Right key | ok | ok | ok |

- The vault bytes were unchanged after the failed attempts.
- SQLite codes alone cannot tell a no-key or wrong-key open from a non-database.
- Today's `from_sqlite` maps it to `Invalid`, so `proves_unwritable()` is true and the switch fence is
  skipped. This confirms HIGH-4 on the real engine.
- The fence cannot even be taken without the key, which is why the design uses the key-independent
  handle fence (A-7.1).
