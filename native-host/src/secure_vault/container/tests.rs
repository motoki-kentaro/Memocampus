use super::*;
use crate::secure_vault::phrase;
use std::io::Cursor;

/// The cheapest parameters a restore accepts, to keep the tests fast.
fn cheap_kdf() -> Kdf {
    Kdf::with_cost(ARGON_M_KIB_MIN, 1, 1).unwrap()
}

fn fixture() -> (VaultKey, RecoverySeed, VaultFacts) {
    let key = VaultKey::generate().unwrap();
    let seed = RecoverySeed::generate().unwrap();
    let vault_id = VaultId::generate().unwrap();
    let recovery_salt = CipherSalt::from_array([7; 16]);
    let recovery = recovery::wrap(&key, &seed, &vault_id, &recovery_salt).unwrap();
    let facts = VaultFacts {
        schema_version: crate::schema::SCHEMA_VERSION,
        vault_id,
        cipher_salt: CipherSalt::from_array([3; 16]),
        recovery_salt,
        recovery,
    };
    (key, seed, facts)
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn package_b(key: &VaultKey, facts: VaultFacts, data: &[u8], policy: Option<&[u8]>) -> Vec<u8> {
    let prepared = PreparedB::new(key, facts, data.len() as u64, policy).unwrap();
    let mut out = Vec::new();
    prepared.write_to(&mut out, &mut Cursor::new(data)).unwrap();
    assert_eq!(out.len() as u64, prepared.total_len());
    out
}

fn restore_b(bytes: &[u8], seed: &RecoverySeed) -> Result<(RestoredB, Vec<u8>), FormatError> {
    let mut sink = Vec::new();
    let restored = read_b(
        &mut Cursor::new(bytes),
        bytes.len() as u64,
        |header| unlock_with_phrase(header, seed),
        &mut sink,
    )?;
    Ok((restored, sink))
}

#[test]
fn format_b_round_trips_with_the_phrase_only() {
    let (key, seed, facts) = fixture();
    let data = payload(3 * CHUNK + 123);
    let policy = br#"{"policy":"CANARY-policy-rule"}"#;
    let bytes = package_b(&key, facts, &data, Some(policy));
    assert_eq!(detect(&bytes[..16]), Detected::Container(Kind::B));
    let (restored, sink) = restore_b(&bytes, &seed).unwrap();
    assert_eq!(sink, data);
    assert!(restored.key.same_as(&key));
    assert_eq!(
        restored.policy.as_deref().map(Vec::as_slice),
        Some(&policy[..])
    );
    // The phrase as a user types it.
    let typed = phrase::parse(&phrase::encode(&seed).to_ascii_uppercase()).unwrap();
    assert!(restore_b(&bytes, &typed).is_ok());
    // Nothing secret in the file: the key in no form, the policy copy only sealed.
    assert!(!key.appears_in(&bytes));
    assert!(!bytes.windows(6).any(|w| w == b"CANARY"));
    assert!(!bytes
        .windows(phrase::encode(&seed).len())
        .any(|w| w == phrase::encode(&seed).as_bytes()));
}

#[test]
fn format_b_refuses_a_wrong_phrase_before_writing_anything() {
    let (key, _, facts) = fixture();
    let bytes = package_b(&key, facts, &payload(4096), None);
    let mut sink = Vec::new();
    let other = RecoverySeed::generate().unwrap();
    let error = read_b(
        &mut Cursor::new(&bytes),
        bytes.len() as u64,
        |header| unlock_with_phrase(header, &other),
        &mut sink,
    )
    .err()
    .unwrap();
    assert_eq!(error, FormatError::PhraseRejected);
    assert!(sink.is_empty());
}

#[test]
fn every_altered_byte_of_a_format_b_package_is_refused() {
    let (key, seed, facts) = fixture();
    let bytes = package_b(&key, facts, &payload(2 * CHUNK + 7), Some(b"{}"));
    let prefix_len = 16 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut positions: Vec<usize> = (0..prefix_len).collect();
    positions.extend((prefix_len..bytes.len()).step_by(997));
    positions.extend(bytes.len() - 64..bytes.len());
    for position in positions {
        let mut altered = bytes.clone();
        altered[position] ^= 0x01;
        assert!(restore_b(&altered, &seed).is_err(), "byte {position}");
    }
}

#[test]
fn a_format_b_package_that_is_cut_or_extended_is_refused_before_any_crypto() {
    let (key, seed, facts) = fixture();
    let bytes = package_b(&key, facts, &payload(CHUNK), None);
    for cut in [1, 31, 32, 1000, bytes.len() - 20] {
        let short = &bytes[..bytes.len() - cut];
        assert_eq!(
            restore_b(short, &seed).err(),
            Some(FormatError::Incomplete),
            "{cut}"
        );
    }
    let mut long = bytes.clone();
    long.extend_from_slice(b"appended");
    assert_eq!(restore_b(&long, &seed).err(), Some(FormatError::Incomplete));
    // A file whose real length is less than the header claims, read with the claimed length.
    let mut sink = Vec::new();
    let error = read_b(
        &mut Cursor::new(&bytes[..bytes.len() - 10]),
        bytes.len() as u64,
        |header| unlock_with_phrase(header, &seed),
        &mut sink,
    )
    .err();
    assert_eq!(error, Some(FormatError::Incomplete));
}

/// Rewrites the JSON header of a container (the rest is kept as is).
fn with_header(bytes: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut header: serde_json::Value = serde_json::from_slice(&bytes[16..16 + len]).unwrap();
    edit(&mut header);
    let header = serde_json::to_vec(&header).unwrap();
    let mut out = bytes[..12].to_vec();
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&bytes[16 + len..]);
    out
}

#[test]
fn crafted_format_b_headers_are_refused_without_allocating() {
    let (key, seed, facts) = fixture();
    let bytes = package_b(&key, facts, &payload(4096), None);
    for (edit, expected) in [
        (
            Box::new(|h: &mut serde_json::Value| h["payloadLen"] = u64::MAX.into())
                as Box<dyn FnOnce(&mut serde_json::Value)>,
            FormatError::Malformed,
        ),
        (
            Box::new(|h| h["payloadLen"] = (1u64 << 40).into()),
            FormatError::Incomplete,
        ),
        (
            Box::new(|h| h["policy"] = json_policy(u64::MAX)),
            FormatError::Malformed,
        ),
        (
            Box::new(|h| h["payload"] = "sqlcipher5".into()),
            FormatError::UnsupportedVersion,
        ),
        (
            Box::new(|h| h["schemaVersion"] = 6.into()),
            FormatError::SchemaMismatch,
        ),
        (Box::new(|h| h["extra"] = 1.into()), FormatError::Malformed),
        (
            Box::new(|h| h["vaultId"] = "../../etc".into()),
            FormatError::Malformed,
        ),
    ] {
        let altered = with_header(&bytes, edit);
        assert_eq!(restore_b(&altered, &seed).err(), Some(expected));
    }
    // A header length of zero, or beyond the limit.
    for len in [0u32, MAX_HEADER + 1, u32::MAX] {
        let mut altered = bytes.clone();
        altered[12..16].copy_from_slice(&len.to_le_bytes());
        assert_eq!(
            restore_b(&altered, &seed).err(),
            Some(FormatError::Malformed)
        );
    }
}

fn json_policy(len: u64) -> serde_json::Value {
    serde_json::json!({ "nonce": "00".repeat(24), "len": len })
}

#[test]
fn detection_never_takes_a_file_for_a_container_it_is_not() {
    assert_eq!(detect(b"SQLite format 3\0rest"), Detected::PlainSqlite);
    assert_eq!(
        detect(&[0x5a; 16]),
        Detected::Unknown,
        "a bare SQLCipher file"
    );
    assert_eq!(detect(&[]), Detected::Unknown);
    let mut head = MAGIC.to_vec();
    head.extend_from_slice(&2u16.to_le_bytes());
    head.extend_from_slice(&[b'B', 0, 1, 0, 0, 0]);
    assert_eq!(detect(&head), Detected::UnsupportedVersion);
    head[8] = 1;
    head[9] = 0;
    head[10] = b'Z';
    assert_eq!(detect(&head), Detected::Unknown);
    head[10] = b'C';
    head[11] = 1;
    assert_eq!(detect(&head), Detected::Unknown, "flags must be zero");
    head[11] = 0;
    assert_eq!(detect(&head), Detected::Container(Kind::C));
    assert_eq!(detect(&head[..12]), Detected::Unknown);
}

fn sections_c(meta: &[u8], policy: Option<&[u8]>, vault: &[u8]) -> Vec<Section> {
    let mut sections = vec![Section {
        kind: SECTION_META.into(),
        len: meta.len() as u64,
    }];
    if let Some(policy) = policy {
        sections.push(Section {
            kind: SECTION_POLICY.into(),
            len: policy.len() as u64,
        });
    }
    sections.push(Section {
        kind: SECTION_VAULT.into(),
        len: vault.len() as u64,
    });
    sections
}

fn container_c(password: &[u8], vault: &[u8], policy: Option<&[u8]>) -> Vec<u8> {
    let meta = serde_json::to_vec(&Meta::plain()).unwrap();
    let mut writer = PasswordWriter::start(
        Vec::new(),
        password,
        Kind::C,
        cheap_kdf(),
        sections_c(&meta, policy, vault),
    )
    .unwrap();
    writer.write_all(&meta).unwrap();
    if let Some(policy) = policy {
        writer.write_all(policy).unwrap();
    }
    // Odd write sizes cross chunk boundaries.
    for piece in vault.chunks(10_007) {
        writer.write_all(piece).unwrap();
    }
    writer.finish().unwrap()
}

struct OpenedC {
    meta: Meta,
    policy: Option<Vec<u8>>,
    vault: Vec<u8>,
}

fn open_c(bytes: &[u8], password: &[u8]) -> Result<OpenedC, FormatError> {
    let (header, mut reader) =
        PasswordReader::open(Cursor::new(bytes), bytes.len() as u64, password, Kind::C)?;
    let mut opened = OpenedC {
        meta: Meta::plain(),
        policy: None,
        vault: Vec::new(),
    };
    for section in &header.sections {
        match section.kind.as_str() {
            SECTION_META => opened.meta = Meta::parse(&read_small(&mut reader, section.len)?)?,
            SECTION_POLICY => {
                opened.policy = Some(read_small(&mut reader, section.len)?.to_vec());
            }
            _ => copy_exact(&mut reader, section.len, &mut opened.vault)?,
        }
    }
    assert!(reader.is_complete());
    Ok(opened)
}

#[test]
fn format_c_round_trips_and_only_the_password_opens_it() {
    let vault = payload(2 * CHUNK + 5000);
    let bytes = container_c(b"correct horse", &vault, Some(b"{\"CANARY\":1}"));
    assert_eq!(detect(&bytes[..16]), Detected::Container(Kind::C));
    let opened = open_c(&bytes, b"correct horse").unwrap();
    assert_eq!(opened.vault, vault);
    assert_eq!(opened.policy.as_deref(), Some(&b"{\"CANARY\":1}"[..]));
    assert_eq!(opened.meta, Meta::plain());
    assert_eq!(
        open_c(&bytes, b"correct horsf").err(),
        Some(FormatError::PasswordRejected)
    );
    assert_eq!(
        open_c(&bytes, b"").err(),
        Some(FormatError::PasswordRejected)
    );
    assert!(!bytes.windows(6).any(|w| w == b"CANARY"));
    assert!(!bytes.windows(13).any(|w| w == b"correct horse"));
    // The vault bytes do not appear in the clear (any 64-byte window).
    assert!(!bytes.windows(64).any(|w| w == &vault[1000..1064]));
}

#[test]
fn exact_multiples_of_the_chunk_size_and_tiny_sections_round_trip() {
    for len in [512, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK] {
        let vault = payload(len);
        let bytes = container_c(b"password", &vault, None);
        assert_eq!(open_c(&bytes, b"password").unwrap().vault, vault, "{len}");
    }
}

/// The ciphertext chunks of a password container: (prefix length, chunk boundaries).
fn chunks(bytes: &[u8]) -> (usize, Vec<std::ops::Range<usize>>) {
    let prefix = 16 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    let mut at = prefix;
    while at < bytes.len() {
        let end = (at + CHUNK + TAG).min(bytes.len());
        out.push(at..end);
        at = end;
    }
    (prefix, out)
}

#[test]
fn removed_reordered_duplicated_or_cut_chunks_are_refused() {
    let vault = payload(4 * CHUNK);
    let bytes = container_c(b"password", &vault, None);
    let (prefix, ranges) = chunks(&bytes);
    assert!(ranges.len() >= 5);
    let full = |range: &std::ops::Range<usize>| range.len() == CHUNK + TAG;
    // Two full chunks swapped.
    let mut swapped = bytes[..prefix].to_vec();
    swapped.extend_from_slice(&bytes[ranges[1].clone()]);
    swapped.extend_from_slice(&bytes[ranges[0].clone()]);
    for range in &ranges[2..] {
        swapped.extend_from_slice(&bytes[range.clone()]);
    }
    assert_eq!(
        open_c(&swapped, b"password").err(),
        Some(FormatError::Tampered)
    );
    // A chunk duplicated in place of another.
    assert!(full(&ranges[1]) && full(&ranges[2]));
    let mut duplicated = bytes.clone();
    duplicated.copy_within(ranges[1].clone(), ranges[2].start);
    assert_eq!(
        open_c(&duplicated, b"password").err(),
        Some(FormatError::Tampered)
    );
    // A chunk removed: the length no longer adds up.
    let mut removed = bytes[..ranges[1].start].to_vec();
    removed.extend_from_slice(&bytes[ranges[2].start..]);
    assert_eq!(
        open_c(&removed, b"password").err(),
        Some(FormatError::Incomplete)
    );
    // The final chunk dropped, and the second-to-last presented as final (a truncation that keeps
    // the length right is impossible without the header, which is authenticated as AAD).
    let cut = &bytes[..ranges[ranges.len() - 1].start];
    assert_eq!(
        open_c(cut, b"password").err(),
        Some(FormatError::Incomplete)
    );
    // Bytes appended.
    let mut long = bytes.clone();
    long.push(0);
    assert_eq!(
        open_c(&long, b"password").err(),
        Some(FormatError::Incomplete)
    );
    // A flipped bit anywhere in the body.
    for position in (prefix..bytes.len()).step_by(4099) {
        let mut altered = bytes.clone();
        altered[position] ^= 0x80;
        assert_eq!(
            open_c(&altered, b"password").err(),
            Some(FormatError::Tampered),
            "{position}"
        );
    }
}

#[test]
fn a_changed_header_or_section_table_is_refused() {
    let vault = payload(CHUNK + 10);
    let bytes = container_c(b"password", &vault, Some(b"{}"));
    // Any change of the header changes the AAD of every chunk (or fails its checks first).
    let reordered = with_header(&bytes, |h| {
        let sections = h["sections"].as_array_mut().unwrap();
        sections.swap(0, 1);
    });
    assert_eq!(
        open_c(&reordered, b"password").err(),
        Some(FormatError::Malformed)
    );
    let check = with_header(&bytes, |h| h["check"] = "00".repeat(32).into());
    assert_eq!(
        open_c(&check, b"password").err(),
        Some(FormatError::PasswordRejected),
        "an altered check value looks like a wrong password, never like success"
    );
    let nonce = with_header(&bytes, |h| h["noncePrefix"] = "11".repeat(19).into());
    assert_eq!(
        open_c(&nonce, b"password").err(),
        Some(FormatError::Tampered)
    );
    for (edit, expected) in [
        (
            Box::new(|h: &mut serde_json::Value| h["sections"][0]["kind"] = "../evil".into())
                as Box<dyn FnOnce(&mut serde_json::Value)>,
            FormatError::Malformed,
        ),
        (
            Box::new(|h| h["sections"][2]["len"] = u64::MAX.into()),
            FormatError::Malformed,
        ),
        (
            Box::new(|h| h["sections"][1]["len"] = (MAX_POLICY_COPY + 1).into()),
            FormatError::Malformed,
        ),
        (
            Box::new(|h| h["sections"] = serde_json::json!([])),
            FormatError::Malformed,
        ),
        (
            Box::new(|h| h["aead"] = "aes-gcm".into()),
            FormatError::UnsupportedVersion,
        ),
        (
            Box::new(|h| h["chunk"] = 1.into()),
            FormatError::UnsupportedVersion,
        ),
    ] {
        assert_eq!(
            open_c(&with_header(&bytes, edit), b"password").err(),
            Some(expected)
        );
    }
}

#[test]
fn crafted_kdf_parameters_are_refused_before_argon2_runs() {
    let bytes = container_c(b"password", &payload(4096), None);
    for (field, value, expected) in [
        (
            "mKib",
            serde_json::json!(ARGON_M_KIB_MAX + 1),
            FormatError::Malformed,
        ),
        ("mKib", serde_json::json!(u32::MAX), FormatError::Malformed),
        ("mKib", serde_json::json!(1024), FormatError::Malformed),
        ("t", serde_json::json!(0), FormatError::Malformed),
        (
            "t",
            serde_json::json!(ARGON_T_MAX + 1),
            FormatError::Malformed,
        ),
        (
            "p",
            serde_json::json!(ARGON_P_MAX + 1),
            FormatError::Malformed,
        ),
        ("p", serde_json::json!(0), FormatError::Malformed),
        ("salt", serde_json::json!("00"), FormatError::Malformed),
        (
            "alg",
            serde_json::json!("scrypt"),
            FormatError::UnsupportedVersion,
        ),
        (
            "version",
            serde_json::json!(16),
            FormatError::UnsupportedVersion,
        ),
    ] {
        let altered = with_header(&bytes, |h| h["kdf"][field] = value);
        let started = std::time::Instant::now();
        assert_eq!(
            open_c(&altered, b"password").err(),
            Some(expected),
            "{field}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "{field}"
        );
    }
}

#[test]
fn a_c_container_is_not_opened_as_d_and_a_b_package_is_not_a_c_container() {
    let bytes = container_c(b"password", &payload(4096), None);
    assert_eq!(
        PasswordReader::open(
            Cursor::new(&bytes),
            bytes.len() as u64,
            b"password",
            Kind::D
        )
        .err(),
        Some(FormatError::UnsupportedFormat)
    );
    let (key, seed, facts) = fixture();
    let b = package_b(&key, facts, &payload(4096), None);
    assert!(PasswordReader::open(Cursor::new(&b), b.len() as u64, b"password", Kind::C).is_err());
    assert_eq!(
        restore_b(&bytes, &seed).err(),
        Some(FormatError::UnsupportedFormat)
    );
}

fn container_d(password: &[u8], key: &VaultKey, facts: VaultFacts, data: &[u8]) -> Vec<u8> {
    let prepared = PreparedB::new(key, facts, data.len() as u64, Some(b"{\"p\":1}")).unwrap();
    let mut writer = PasswordWriter::start(
        Vec::new(),
        password,
        Kind::D,
        cheap_kdf(),
        vec![Section {
            kind: SECTION_PACKAGE.into(),
            len: prepared.total_len(),
        }],
    )
    .unwrap();
    prepared
        .write_to(&mut writer, &mut Cursor::new(data))
        .unwrap();
    writer.finish().unwrap()
}

fn open_d(
    bytes: &[u8],
    password: &[u8],
    seed: &RecoverySeed,
) -> Result<(RestoredB, Vec<u8>), FormatError> {
    let (header, mut reader) =
        PasswordReader::open(Cursor::new(bytes), bytes.len() as u64, password, Kind::D)?;
    let mut sink = Vec::new();
    let restored = read_b(
        &mut reader,
        header.sections[0].len,
        |b| unlock_with_phrase(b, seed),
        &mut sink,
    )?;
    if !reader.is_complete() {
        return Err(FormatError::Tampered);
    }
    Ok((restored, sink))
}

#[test]
fn format_d_needs_both_the_password_and_the_phrase() {
    let (key, seed, facts) = fixture();
    let data = payload(2 * CHUNK + 999);
    let bytes = container_d(b"outer password", &key, facts, &data);
    assert_eq!(detect(&bytes[..16]), Detected::Container(Kind::D));
    let (restored, sink) = open_d(&bytes, b"outer password", &seed).unwrap();
    assert_eq!(sink, data);
    assert!(restored.key.same_as(&key));
    assert_eq!(
        restored.policy.as_deref().map(Vec::as_slice),
        Some(&b"{\"p\":1}"[..])
    );
    // The password alone: the phrase is still asked for.
    let other = RecoverySeed::generate().unwrap();
    assert_eq!(
        open_d(&bytes, b"outer password", &other).err(),
        Some(FormatError::PhraseRejected)
    );
    // The phrase alone does nothing against the outer layer.
    assert_eq!(
        open_d(&bytes, b"wrong password", &seed).err(),
        Some(FormatError::PasswordRejected)
    );
    // The inner package's header (vault id, envelope) is not readable from the file.
    assert!(!bytes.windows(10).any(|w| w == b"sqlcipher4"));
    assert!(!key.appears_in(&bytes));
    for position in (100..bytes.len()).step_by(5003) {
        let mut altered = bytes.clone();
        altered[position] ^= 0x04;
        assert!(
            open_d(&altered, b"outer password", &seed).is_err(),
            "{position}"
        );
    }
}

#[test]
fn writers_refuse_more_or_less_than_announced() {
    let (key, _, facts) = fixture();
    let prepared = PreparedB::new(&key, facts, 1000, None).unwrap();
    assert!(prepared
        .write_to(&mut Vec::new(), &mut Cursor::new(vec![0u8; 999]))
        .is_err());
    assert!(prepared
        .write_to(&mut Vec::new(), &mut Cursor::new(vec![0u8; 1001]))
        .is_err());
    let meta = serde_json::to_vec(&Meta::plain()).unwrap();
    let vault = payload(1024);
    let mut writer = PasswordWriter::start(
        Vec::new(),
        b"password",
        Kind::C,
        cheap_kdf(),
        sections_c(&meta, None, &vault),
    )
    .unwrap();
    writer.write_all(&meta).unwrap();
    writer.write_all(&vault[..1000]).unwrap();
    assert!(writer.write_all(&[0u8; 100]).is_err());
    let mut writer = PasswordWriter::start(
        Vec::new(),
        b"password",
        Kind::C,
        cheap_kdf(),
        sections_c(&meta, None, &vault),
    )
    .unwrap();
    writer.write_all(&meta).unwrap();
    assert!(writer.finish().is_err());
    // Out-of-bounds parameters are refused when writing too.
    assert!(PasswordWriter::start(
        Vec::new(),
        b"password",
        Kind::C,
        Kdf::with_cost(ARGON_M_KIB_MAX * 2, 1, 1).unwrap(),
        sections_c(&meta, None, &vault),
    )
    .is_err());
}

#[test]
fn every_backup_gets_a_new_salt_and_nonce() {
    let vault = payload(4096);
    let a = container_c(b"password", &vault, None);
    let b = container_c(b"password", &vault, None);
    assert_ne!(a, b);
    let header = |bytes: &[u8]| -> HeaderP {
        let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        serde_json::from_slice(&bytes[16..16 + len]).unwrap()
    };
    assert_ne!(header(&a).kdf.salt, header(&b).kdf.salt);
    assert_ne!(header(&a).nonce_prefix, header(&b).nonce_prefix);
}
