# Third-party notices for the native binaries

`memocampus-native-host.exe` and `memocampus-mcp.exe` (Issue #51, Slice B) contain the following
third-party software, compiled in statically. It is **not** covered by Memocampus' MIT License; its
own license applies. (The browser extension does not contain it; its notices are in
`THIRD_PARTY_NOTICES.md`.)

## SQLCipher Community Edition 4.5.7

| | |
|---|---|
| What | SQLite 3.45.3 with the SQLCipher encryption extension, as bundled by the Rust crate `libsqlite3-sys` 0.30.1 (feature `bundled-sqlcipher-vendored-openssl`) |
| Provider | Zetetic LLC (SQLCipher); SQLite is in the public domain |
| Source | https://github.com/sqlcipher/sqlcipher (bundled copy: `libsqlite3-sys-0.30.1/sqlcipher/sqlite3.c`) |
| License | BSD-style, full text in `LICENSES/SQLCipher.txt` (copied unchanged from `libsqlite3-sys-0.30.1/sqlcipher/LICENSE`) |
| Notice | Copyright (c) 2008-2020 Zetetic LLC. All rights reserved. |
| Changes by Memocampus | None |

## OpenSSL 3.6.3

| | |
|---|---|
| What | libcrypto, used by SQLCipher for AES-256 and HMAC-SHA512, built from source by the Rust crate `openssl-src` 300.6.1+3.6.3 |
| Provider | The OpenSSL Project Authors |
| Source | https://www.openssl.org/ (bundled copy: `openssl-src-300.6.1+3.6.3/openssl`) |
| License | Apache License, Version 2.0, full text in `LICENSES/OpenSSL.txt` (copied unchanged from the OpenSSL source) |
| Changes by Memocampus | None |

The Rust crates used for key handling (`bip39`, `chacha20poly1305`, `hkdf`, `hmac`, `sha2`,
`subtle`, `zeroize`, `getrandom`) are licensed under MIT and/or Apache-2.0 (`bip39`: CC0-1.0), like
the crates already used before Slice B; see each crate's metadata in `Cargo.lock`.

This file is not legal advice.
