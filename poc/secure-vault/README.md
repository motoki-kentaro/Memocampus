# Issue #51 Slice A — isolated proof of concept

Not production code. Each directory is its own Cargo workspace, and no production crate depends on
it. Production crates are used read-only, as path dependencies. All tests use synthetic data in fresh
temp directories.

Build into a scratch target directory:

    cd poc/secure-vault/crypto-poc
    cargo test --release --target-dir <scratch> -- --nocapture --test-threads=1

`cipher-poc`, `size-cipher`:

- Build SQLCipher with a vendored OpenSSL.
- On Windows they need a native Perl, for example MSYS2 `mingw-w64-x86_64-perl`:

      PERL=C:\msys64\mingw64\bin\perl.exe PATH=/c/msys64/mingw64/bin:$PATH cargo test --release ...

`size-plain`: the plain SQLite baseline.

Results are in `docs/secure-vault/poc-results.md`.
