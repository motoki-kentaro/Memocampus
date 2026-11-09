//! Issue #51 Slice A PoCs that do not need SQLCipher. Run: `cargo test --release -- --nocapture --test-threads=1`.
//! Every test uses a fresh temp directory and synthetic data; nothing touches a real vault, policy,
//! DPAPI store entry or Chrome profile.

use memocampus_native_host::schema;
use rusqlite::{backup::Backup, Connection, OpenFlags};
use secure_vault_poc::*;
use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mc-sv-poc-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const MARKER_URL: &str = "https://secret-marker.example/very-private-page";
const MARKER_POLICY: &str = "neverSave:bank-marker.example";

/// A schema 7 vault made by the production `create_sql`, with visits and one image blob.
fn plain_vault(path: &Path, visits: usize) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(&format!("BEGIN; {} COMMIT;", schema::create_sql())).unwrap();
    for i in 0..visits {
        db.execute(
            "INSERT INTO visits VALUES (?1, ?2, 't', '2026-11-09T00:00:00.000Z')",
            [format!("v{i}"), format!("{MARKER_URL}/{i}")],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO image_blobs (id, sha256, mime_type, byte_length, width, height, created_at, last_saved_at, data)
         VALUES (1, ?1, 'image/png', 4, 1, 1, 0, 0, x'89504e47')",
        ["a".repeat(64)],
    )
    .unwrap();
}

/// Order-independent digest of every row of every table.
fn content(db: &Connection) -> Vec<(String, String)> {
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|t| {
            let mut stmt = db.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
            let n = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |r| {
                    Ok((0..n).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>().join("|"))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows.sort();
            (t, hex(&<sha2::Sha256 as sha2::Digest>::digest(rows.join("\n"))))
        })
        .collect()
}

fn ids(db: &Connection) -> (i64, i64) {
    (
        db.query_row("PRAGMA application_id", [], |r| r.get(0)).unwrap(),
        db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap(),
    )
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect()
}

// ------------------------------------------------------------------------------------------ A-2

#[test]
fn a2_1_csprng_vault_keys() {
    let keys: BTreeSet<[u8; 32]> = (0..10_000).map(|_| random::<32>()).collect();
    assert_eq!(keys.len(), 10_000, "no repeats among 10,000 keys");
    assert!(!keys.contains(&[0u8; 32]));
    let ones: u32 = keys.iter().flat_map(|k| k.iter()).map(|b| b.count_ones()).sum();
    let ratio = ones as f64 / (10_000.0 * 256.0);
    println!("A-2.1 BCryptGenRandom: 10000 keys distinct, bit ratio {ratio:.4}");
    assert!((0.49..0.51).contains(&ratio));
}

#[test]
fn a2_2_dpapi_user_scope_roundtrip_and_failures() {
    let vk: VaultKey = random();
    let label = b"memocampus/vault-key/v1\0vault-id-0123456789abcdef";
    let t = Instant::now();
    let blob = dpapi::protect(&vk, label).unwrap();
    let protect_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(!contains(&blob, &vk), "the key is not in the blob in clear");
    let t = Instant::now();
    assert_eq!(dpapi::unprotect(&blob, label).unwrap(), vk);
    let unprotect_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("A-2.2 DPAPI blob {} bytes, protect {protect_ms:.2} ms, unprotect {unprotect_ms:.2} ms", blob.len());

    // Wrong purpose label (entropy).
    let wrong = dpapi::unprotect(&blob, b"another label").unwrap_err();
    // Corruption at many positions, and truncation.
    let mut codes = BTreeSet::new();
    let mut accepted = 0;
    let mut accepted_at = Vec::new();
    for pos in (0..blob.len()).step_by(3) {
        let mut bad = blob.clone();
        bad[pos] ^= 0x01;
        match dpapi::unprotect(&bad, label) {
            Ok(out) => {
                accepted += 1;
                accepted_at.push((pos, out == vk));
            }
            Err(code) => {
                codes.insert(code);
            }
        }
    }
    let truncated = dpapi::unprotect(&blob[..blob.len() - 1], label).unwrap_err();
    let empty = dpapi::unprotect(&[], label).unwrap_err();
    println!(
        "A-2.2 wrong entropy -> error {wrong}; {} single-bit corruptions -> {accepted} accepted, errors {codes:?}; truncated -> {truncated}; empty -> {empty}",
        blob.len().div_ceil(3)
    );
    // Finding: DPAPI does not authenticate every byte of its blob (some header fields are not
    // covered). What matters is that it never yields another key: every accepted variant must
    // decrypt to the same VK. The key store adds its own key-check value on top (see architecture).
    println!("A-2.2 accepted corrupted positions (pos, same VK): {accepted_at:?}");
    assert!(accepted_at.iter().all(|(_, same)| *same), "a corrupted blob yielded a different key");
    // Exhaustive single-bit flips over the whole blob.
    let mut exhaustive = (0, 0, 0);
    for pos in 0..blob.len() {
        for bit in 0..8 {
            let mut bad = blob.clone();
            bad[pos] ^= 1 << bit;
            match dpapi::unprotect(&bad, label) {
                Ok(out) if out == vk => exhaustive.0 += 1,
                Ok(_) => exhaustive.1 += 1,
                Err(_) => exhaustive.2 += 1,
            }
        }
    }
    println!("A-2.2 exhaustive {} single-bit flips: same VK {}, different output {}, refused {}", blob.len() * 8, exhaustive.0, exhaustive.1, exhaustive.2);
    assert_eq!(exhaustive.1, 0);
}

#[test]
fn a2_3_new_pc_restore_rewraps_with_dpapi() {
    // PC 1: VK, phrase, recovery envelope, DPAPI blob.
    let vk: VaultKey = random();
    let vault_id: [u8; 16] = random();
    let entropy: [u8; 16] = random();
    let envelope = wrap_for_recovery(&vk, &entropy, &vault_id);
    let label = [b"memocampus/vault-key/v1\0".as_slice(), &vault_id].concat();
    let blob_pc1 = dpapi::protect(&vk, &label).unwrap();
    // "PC 2": the DPAPI blob is not available (modelled by discarding it). The user types the phrase.
    drop(blob_pc1.clone());
    let typed = phrase_of(&entropy).to_uppercase().replace(' ', "   ");
    let recovered = unwrap_recovery(&envelope, &parse_phrase(&typed).unwrap(), &vault_id).unwrap();
    assert_eq!(recovered, vk);
    let blob_pc2 = dpapi::protect(&recovered, &label).unwrap();
    assert_ne!(blob_pc1, blob_pc2);
    assert_eq!(dpapi::unprotect(&blob_pc2, &label).unwrap(), vk);
    println!("A-2.3 phrase -> VK -> fresh DPAPI blob: ok (same machine/user; a real second PC is NOT RUN)");
}

// ------------------------------------------------------------------------------------------ A-3

#[test]
fn a3_1_bip39_official_vectors_and_wordlist() {
    for (entropy, phrase) in [
        ("00000000000000000000000000000000", "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"),
        ("7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f", "legal winner thank year wave sausage worth useful legal winner thank yellow"),
        ("80808080808080808080808080808080", "letter advice cage absurd amount doctor acoustic avoid letter advice cage above"),
        ("ffffffffffffffffffffffffffffffff", "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong"),
    ] {
        let e: [u8; 16] = unhex(entropy).unwrap().try_into().unwrap();
        assert_eq!(phrase_of(&e), phrase);
        assert_eq!(parse_phrase(phrase).unwrap(), e);
    }
    let list = bip39::Language::English.word_list();
    assert_eq!(list.len(), 2048);
    let prefixes: BTreeSet<&str> = list.iter().map(|w| &w[..w.len().min(4)]).collect();
    assert_eq!(prefixes.len(), 2048, "4-letter prefixes are unique");
    println!("A-3.1 BIP-39 vectors ok; 2048 words; 4-letter prefixes unique; 12 words = 128-bit entropy + 4-bit checksum");
}

#[test]
fn a3_2_typos_and_swaps_never_unlock() {
    let vk: VaultKey = random();
    let vault_id: [u8; 16] = random();
    let entropy: [u8; 16] = random();
    let envelope = wrap_for_recovery(&vk, &entropy, &vault_id);
    let words: Vec<String> = phrase_of(&entropy).split(' ').map(String::from).collect();
    let list = bip39::Language::English.word_list();

    let (mut tried, mut checksum_ok, mut unlocked) = (0, 0, 0);
    for pos in 0..12 {
        for w in list.iter().filter(|w| **w != words[pos]) {
            let mut v = words.clone();
            v[pos] = w.to_string();
            tried += 1;
            if let Ok(e) = parse_phrase(&v.join(" ")) {
                checksum_ok += 1;
                if unwrap_recovery(&envelope, &e, &vault_id).is_ok() {
                    unlocked += 1;
                }
            }
        }
    }
    let (mut swaps, mut swap_checksum_ok, mut swap_unlocked) = (0, 0, 0);
    for pos in 0..11 {
        if words[pos] == words[pos + 1] {
            continue;
        }
        let mut v = words.clone();
        v.swap(pos, pos + 1);
        swaps += 1;
        if let Ok(e) = parse_phrase(&v.join(" ")) {
            swap_checksum_ok += 1;
            if unwrap_recovery(&envelope, &e, &vault_id).is_ok() {
                swap_unlocked += 1;
            }
        }
    }
    let mut short = words.clone();
    short.pop();
    assert_eq!(parse_phrase(&short.join(" ")), Err(PhraseError::WordCount(11)));
    let mut unknown = words.clone();
    unknown[4] = "memocampus".into();
    assert_eq!(parse_phrase(&unknown.join(" ")), Err(PhraseError::UnknownWord(5)));
    println!(
        "A-3.2 single-word substitutions: {tried} tried, {checksum_ok} pass checksum ({:.2}%), {unlocked} unlock; adjacent swaps: {swaps} tried, {swap_checksum_ok} pass checksum, {swap_unlocked} unlock",
        100.0 * checksum_ok as f64 / tried as f64
    );
    assert_eq!(unlocked + swap_unlocked, 0);
}

#[test]
fn a3_3_envelope_binding_and_regeneration() {
    let vk: VaultKey = random();
    let vault_id: [u8; 16] = random();
    let old: [u8; 16] = random();
    let old_env = wrap_for_recovery(&vk, &old, &vault_id); // also inside every older backup
    // Wrong vault id / altered fields.
    assert_eq!(unwrap_recovery(&old_env, &old, &random()), Err(UnwrapError::Rejected));
    let mut bad = old_env.clone();
    bad.salt.replace_range(0..2, if &bad.salt[0..2] == "00" { "01" } else { "00" });
    assert_eq!(unwrap_recovery(&bad, &old, &vault_id), Err(UnwrapError::Rejected));
    let mut bad = old_env.clone();
    bad.scheme = "bip39-128/v0".into();
    assert_eq!(unwrap_recovery(&bad, &old, &vault_id), Err(UnwrapError::Unsupported));
    // Regeneration: the current VK is re-wrapped under a new phrase; VK is unchanged.
    let new: [u8; 16] = random();
    let new_env = wrap_for_recovery(&vk, &new, &vault_id);
    assert_eq!(unwrap_recovery(&new_env, &new, &vault_id).unwrap(), vk);
    assert_eq!(unwrap_recovery(&old_env, &old, &vault_id).unwrap(), vk, "an old backup still opens with the old phrase");
    assert_eq!(unwrap_recovery(&old_env, &new, &vault_id), Err(UnwrapError::Rejected), "the new phrase does not open old backups");
    assert_eq!(unwrap_recovery(&new_env, &old, &vault_id), Err(UnwrapError::Rejected), "the old phrase no longer opens the current wrap");
    println!("A-3.3 regeneration: VK unchanged; old backups need the old phrase; not a revocation");
}

// ------------------------------------------------------------------------------------------ A-5

fn package_b(vk: &VaultKey, vault_id: &[u8; 16], entropy: &[u8; 16], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_b(&mut out, vk, vault_id, wrap_for_recovery(vk, entropy, vault_id), payload, Some(MARKER_POLICY.as_bytes())).unwrap();
    out
}

#[test]
fn a5_1_format_b_roundtrip_and_fail_closed() {
    let (vk, vault_id, entropy): (VaultKey, [u8; 16], [u8; 16]) = (random(), random(), random());
    // Stand-in payload (a SQLCipher file is random-looking bytes); SQLCipher itself: see A-1/A-4.
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let file = package_b(&vk, &vault_id, &entropy, &payload);
    assert_eq!(detect(&file[..16]), Detected::Container { kind: b'B' });
    assert!(!contains(&file, MARKER_POLICY.as_bytes()), "policy copy is not in clear");
    let mut sink = Vec::new();
    let got = read_b(&mut Cursor::new(&file), file.len() as u64, &entropy, &mut sink).unwrap();
    assert_eq!((got.vk, got.vault_id, sink == payload), (vk, vault_id, true));
    assert_eq!(got.policy.as_deref(), Some(MARKER_POLICY.as_bytes()));

    let read = |bytes: &[u8], e: &[u8; 16]| read_b(&mut Cursor::new(bytes), bytes.len() as u64, e, &mut Vec::new()).map(|_| ());
    assert_eq!(read(&file, &random()), Err(RestoreError::PhraseRejected));
    assert_eq!(read(&file[..file.len() - 1], &entropy), Err(RestoreError::Incomplete));
    assert_eq!(read(&[file.as_slice(), &[0]].concat(), &entropy), Err(RestoreError::Incomplete));
    let mut v = file.clone();
    v[8] = 2;
    assert_eq!(read(&v, &entropy), Err(RestoreError::UnsupportedVersion));
    let mut v = file.clone();
    v[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(read(&v, &entropy), Err(RestoreError::Malformed));
    // Every 97th byte flipped: never accepted.
    let mut outcomes = std::collections::BTreeMap::new();
    for pos in (0..file.len()).step_by(97) {
        let mut v = file.clone();
        v[pos] ^= 0x20;
        let r = read(&v, &entropy);
        assert!(r.is_err(), "flip at {pos} accepted");
        *outcomes.entry(format!("{:?}", r.unwrap_err())).or_insert(0) += 1;
    }
    println!("A-5.1 format B {} bytes (+{} overhead); flips -> {outcomes:?}", file.len(), file.len() - payload.len());
}

#[test]
fn a5_2_format_c_over_a_plain_vault_restores_into_a_new_vault_only() {
    let tmp = Tmp::new("c-a");
    let src = tmp.0.join("source.sqlite3");
    plain_vault(&src, 500);
    let raw = fs::read(&src).unwrap();
    assert_eq!(detect(&raw[..16]), Detected::PlainSqlite);

    let password = "correct horse battery staple 2026";
    let t = Instant::now();
    let mut file = Vec::new();
    write_c(&mut file, password, default_kdf(), &[("policy", MARKER_POLICY.as_bytes()), ("vault-a", &raw)]).unwrap();
    let write_ms = t.elapsed().as_millis();
    assert_eq!(detect(&file[..16]), Detected::Container { kind: b'C' });
    assert!(!contains(&file, MARKER_URL.as_bytes()) && !contains(&file, MARKER_POLICY.as_bytes()));
    assert!(!contains(&file, b"SQLite format 3"));

    // Restore: a brand-new file in the chosen folder, never the source; the policy copy is handed back
    // to the caller (to show and compare), never written over a global policy here.
    let restore_dir = tmp.0.join("restore");
    fs::create_dir(&restore_dir).unwrap();
    let partial = restore_dir.join(".restore-1.partial");
    let mut target = fs::OpenOptions::new().write(true).create_new(true).open(&partial).unwrap();
    let mut policy = Vec::new();
    let t = Instant::now();
    read_c(&mut Cursor::new(&file), file.len() as u64, password, &mut |kind, bytes| {
        use std::io::Write;
        match kind {
            "vault-a" => target.write_all(bytes).map_err(|_| RestoreError::Io),
            "policy" => Ok(policy.extend_from_slice(bytes)),
            _ => Err(RestoreError::UnsupportedFormat),
        }
    })
    .unwrap();
    let read_ms = t.elapsed().as_millis();
    target.sync_all().unwrap();
    drop(target);
    assert_eq!(policy, MARKER_POLICY.as_bytes());
    let restored = Connection::open_with_flags(&partial, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    let check: String = restored.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    assert_eq!(check, "ok");
    schema::check(&restored).unwrap();
    let source = Connection::open(&src).unwrap();
    assert_eq!(ids(&restored), ids(&source));
    assert_eq!(content(&restored), content(&source));
    drop(restored);
    let published = restore_dir.join("restored.sqlite3");
    assert!(!published.exists());
    fs::rename(&partial, &published).unwrap();
    assert_eq!(files(&restore_dir), BTreeSet::from(["restored.sqlite3".to_string()]));
    println!("A-5.2 format C(A): {} -> {} bytes; write {write_ms} ms, restore {read_ms} ms (Argon2id m=64MiB t=3 p=1)", raw.len(), file.len());

    // Fail-closed cases.
    let read = |bytes: &[u8], pw: &str| read_c(&mut Cursor::new(bytes), bytes.len() as u64, pw, &mut |_, _| Ok(())).map(|_| ());
    assert_eq!(read(&file, "wrong password"), Err(RestoreError::PasswordRejected));
    assert_eq!(read(&file[..file.len() - 1], password), Err(RestoreError::Incomplete));
    let header_len = u32::from_le_bytes(file[12..16].try_into().unwrap()) as usize;
    let body = 16 + header_len;
    let mut v = file.clone();
    v[body + 2 * (CHUNK + 16) + 5] ^= 1; // inside the 3rd (last) chunk
    assert_eq!(read(&v, password), Err(RestoreError::Tampered));
    let mut v = file.clone(); // swap chunks 0 and 1 (both full, same length)
    assert!(raw.len() > 2 * CHUNK);
    let (a, b) = (body, body + (CHUNK + 16));
    let chunk1 = v[a..b].to_vec();
    v.copy_within(b..b + CHUNK + 16, a);
    v[b..b + CHUNK + 16].copy_from_slice(&chunk1);
    assert_eq!(read(&v, password), Err(RestoreError::Tampered));
    let mut v = file.clone(); // header byte (inside the JSON): AAD mismatch or bad JSON
    v[20] ^= 1;
    assert!(read(&v, password).is_err());
    // Oversized KDF parameters are refused before Argon2 allocates.
    let hostile = {
        let mut kdf = default_kdf();
        kdf.m_kib = u32::MAX;
        kdf
    };
    let t = Instant::now();
    assert_eq!(password_key("x", &hostile), Err(RestoreError::Malformed));
    assert!(t.elapsed() < Duration::from_millis(50));
}

#[test]
fn a5_3_format_c_over_b_needs_password_and_phrase() {
    let (vk, vault_id, entropy): (VaultKey, [u8; 16], [u8; 16]) = (random(), random(), random());
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(40503) >> 7) as u8).collect();
    let b = package_b(&vk, &vault_id, &entropy, &payload);
    let password = "backup password";
    let mut c = Vec::new();
    write_c(&mut c, password, default_kdf(), &[("package-b", &b)]).unwrap();

    // Phrase alone: the file is a C container, B's reader refuses it.
    let as_b = read_b(&mut Cursor::new(&c), c.len() as u64, &entropy, &mut Vec::new());
    assert_eq!(as_b.err(), Some(RestoreError::UnsupportedFormat));
    // Password alone: yields the inner B package, which is still SQLCipher + phrase-wrapped VK.
    let mut inner = Vec::new();
    read_c(&mut Cursor::new(&c), c.len() as u64, password, &mut |kind, bytes| {
        assert_eq!(kind, "package-b");
        inner.extend_from_slice(bytes);
        Ok(())
    })
    .unwrap();
    assert_eq!(inner, b);
    let wrong_phrase = read_b(&mut Cursor::new(&inner), inner.len() as u64, &random(), &mut Vec::new());
    assert_eq!(wrong_phrase.err(), Some(RestoreError::PhraseRejected));
    // Both: VK and payload.
    let mut sink = Vec::new();
    let got = read_b(&mut Cursor::new(&inner), inner.len() as u64, &entropy, &mut sink).unwrap();
    assert_eq!((got.vk, sink == payload), (vk, true));
    println!("A-5.3 C(B): password alone -> still encrypted B; phrase alone -> refused; both -> VK");
}

#[test]
fn a5_4_argon2id_cost() {
    let kdf = default_kdf();
    let t = Instant::now();
    password_key("pw", &kdf).unwrap();
    println!("A-5.4 Argon2id m=64MiB t=3 p=1: {} ms", t.elapsed().as_millis());
}

// ------------------------------------------------------------------------------------------ A-4 (plain only)

#[test]
fn a4_1_plain_to_plain_backup_api_and_vacuum_into() {
    let tmp = Tmp::new("plain");
    let src = tmp.0.join("vault.sqlite3");
    plain_vault(&src, 1000);
    let source = Connection::open(&src).unwrap();
    // A second connection writing meanwhile must not tear the snapshot (Backup API restarts or
    // copies one snapshot inside a read transaction, as production's `copy_and_verify`).
    let writer = Connection::open(&src).unwrap();

    let api = tmp.0.join("api.sqlite3");
    let mut dest = Connection::open(&api).unwrap();
    source.execute_batch("BEGIN").unwrap();
    let before = content(&source);
    {
        let backup = Backup::new(&source, &mut dest).unwrap();
        // The writer is locked out of committing while the read transaction holds SHARED.
        writer.busy_timeout(Duration::from_millis(50)).unwrap();
        let w = writer.execute("INSERT INTO visits VALUES ('late', 'https://late/', 't', 'x')", []);
        backup.run_to_completion(1000, Duration::ZERO, None).unwrap();
        println!("A-4.1 concurrent writer during Backup API snapshot -> {:?}", w.map_err(|e| e.sqlite_error_code()));
    }
    source.execute_batch("COMMIT").unwrap();
    let check: String = dest.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    assert_eq!(check, "ok");
    schema::check(&dest).unwrap();
    assert_eq!(ids(&dest), (schema::APP_ID, schema::SCHEMA_VERSION));
    assert_eq!(content(&dest), before);

    let vac = tmp.0.join("vacuum.sqlite3");
    source.execute("VACUUM INTO ?1", [vac.to_str().unwrap()]).unwrap();
    let v = Connection::open(&vac).unwrap();
    schema::check(&v).unwrap();
    assert_eq!(ids(&v), (schema::APP_ID, schema::SCHEMA_VERSION));
    assert_eq!(content(&v), content(&source));
    drop((v, dest, source, writer));
    let left: Vec<String> = files(&tmp.0).into_iter().filter(|f| f.contains("-journal") || f.contains("-wal")).collect();
    println!("A-4.1 Backup API + VACUUM INTO: integrity ok, schema::check ok, ids kept, content equal; leftover journals {left:?}");
    assert!(left.is_empty());
}

// ------------------------------------------------------------------------------------------ A-7 / A-8

#[test]
fn a7_1_exclusive_handle_fence_needs_no_key() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = Tmp::new("fence");
    let path = tmp.0.join("vault.sqlite3");
    plain_vault(&path, 1);
    let exclusive = || fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&path);

    let writer = Connection::open(&path).unwrap();
    let busy = exclusive().unwrap_err().raw_os_error();
    drop(writer);
    let fence = exclusive().unwrap();
    let blocked = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .and_then(|db| db.query_row("SELECT count(*) FROM visits", [], |r| r.get::<_, i64>(0)));
    let blocked = blocked.unwrap_err().sqlite_error_code();
    drop(fence);
    assert_eq!(Connection::open(&path).unwrap().query_row("SELECT count(*) FROM visits", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    println!("A-7.1 exclusive handle while a connection is open -> os error {busy:?}; open while fenced -> {blocked:?}");
    assert_eq!(busy, Some(32));
    assert_ne!(blocked, Some(rusqlite::ErrorCode::NotADatabase));
}

#[test]
fn a8_1_an_encrypted_looking_vault_is_reported_as_not_a_database_and_by_visible_store_as_schema() {
    let tmp = Tmp::new("enc");
    let path = tmp.0.join("vault.sqlite3");
    // What a plain SQLite build (or a build without the key) sees of a SQLCipher file: random pages.
    let mut bytes = vec![0u8; 8 * 4096];
    os_random(&mut bytes);
    fs::write(&path, &bytes).unwrap();
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    let err = db.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0)).unwrap_err();
    let (code, ext) = match &err {
        rusqlite::Error::SqliteFailure(e, _) => (e.code, e.extended_code),
        other => panic!("{other:?}"),
    };
    println!("A-8.1 plain SQLite on encrypted-looking file -> {code:?} (extended {ext})");
    assert_eq!(code, rusqlite::ErrorCode::NotADatabase);
    assert_eq!(fs::read(&path).unwrap(), bytes, "nothing was written");

    // VisibleStore (production, unchanged) on the same file.
    let app = tmp.0.join("app");
    fs::create_dir(&app).unwrap();
    let store = memocampus_visible_store::VisibleStore::new(path.clone(), app).unwrap();
    let visible = store.check_vault().unwrap_err().code();
    println!("A-8.1 VisibleStore::check_vault on it -> {visible}");
    assert_eq!(visible, "vault_schema_mismatch");
}

#[test]
fn a4_2_raw_byte_copy_under_a_shared_lock_is_a_consistent_snapshot() {
    // Rollback-journal mode (the vault's only mode; VisibleStore refuses WAL). While a read transaction
    // holds SHARED, no writer can commit, so the main file's bytes are one consistent snapshot. For a
    // SQLCipher vault the same copy stays ciphertext: no plaintext temp file is ever made.
    let tmp = Tmp::new("raw");
    let src = tmp.0.join("vault.sqlite3");
    plain_vault(&src, 2000);
    let reader = Connection::open(&src).unwrap();
    let writer = Connection::open(&src).unwrap();
    writer.busy_timeout(Duration::from_millis(100)).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let expected = content(&reader); // takes SHARED
    let quick: String = reader.query_row("PRAGMA quick_check", [], |r| r.get(0)).unwrap();
    // A writer can still RESERVE and write into its cache, but cannot commit (needs EXCLUSIVE).
    let attempt = writer.execute_batch("BEGIN IMMEDIATE; INSERT INTO visits VALUES ('w', 'https://w/', 't', 'x'); COMMIT;");
    let bytes = fs::read(&src).unwrap(); // the file is shared for reading by SQLite
    reader.execute_batch("COMMIT").unwrap();
    let _ = writer.execute_batch("ROLLBACK");
    let copy = tmp.0.join("copy.sqlite3");
    fs::write(&copy, &bytes).unwrap();
    let c = Connection::open(&copy).unwrap();
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    schema::check(&c).unwrap();
    assert_eq!((quick.as_str(), integrity.as_str()), ("ok", "ok"));
    assert_eq!(content(&c), expected);
    println!(
        "A-4.2 raw copy under SHARED: {} bytes, writer commit during copy -> {:?}, copy integrity ok, schema ok, content equal",
        bytes.len(),
        attempt.as_ref().map_err(|e| e.sqlite_error_code())
    );
    assert!(attempt.is_err(), "no commit while the snapshot is read");
}

/// Writes a vault with the plain SQLite build for the SQLCipher PoC's regression (C-2). No-op unless
/// MC_PLAIN_FIXTURE is set.
#[test]
fn fixture_for_cipher_poc() {
    if let Ok(path) = std::env::var("MC_PLAIN_FIXTURE") {
        let _ = fs::remove_file(&path);
        plain_vault(Path::new(&path), 300);
    }
}
