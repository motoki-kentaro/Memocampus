//! Synthetic data, temporary vaults and temporary folders only. The Save dialog is replaced by a
//! closure (its answer, and whatever happens while it is "open"); file I/O is the real thing.
use super::{file_name, HOOK};
use crate::{
    dialog::Dialogs,
    page_manage::Manager,
    pages,
    vault::{self, tests::TempDir, ConfigLock, Session},
};
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

struct Pick<F: Fn(&str) -> Option<PathBuf>>(F);
impl<F: Fn(&str) -> Option<PathBuf>> Dialogs for Pick<F> {
    fn choose_new_vault(&self, _: &Path, _: &str) -> Option<PathBuf> {
        None
    }
    fn choose_existing_vault(&self, _: Option<&Path>) -> Option<PathBuf> {
        None
    }
    fn choose_backup_dir(&self) -> Option<PathBuf> {
        None
    }
    fn choose_export_file(&self, _: &str, _: &str) -> Option<PathBuf> {
        None
    }
    fn choose_page_export_file(&self, name: &str) -> Option<PathBuf> {
        (self.0)(name)
    }
}
/// Removes the fault hook when a test ends, even by panic.
struct Hook;
impl Hook {
    fn set(hook: impl FnMut(&'static str, u64) -> bool + 'static) -> Self {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        Self
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        HOOK.with(|slot| *slot.borrow_mut() = None);
    }
}

struct Saved {
    id: i64,
    save_id: String,
    sha: String,
    bytes: Vec<u8>,
}
struct Fixture {
    tmp: TempDir,
    session: Session,
    manager: Manager,
    binding: Value,
    out: PathBuf,
}
fn activate(tmp: &TempDir, name: &str) -> PathBuf {
    let path = tmp.0.join(name);
    if !path.exists() {
        vault::create_vault(&path).unwrap();
    }
    let env = tmp.env();
    let lock = ConfigLock::acquire(&env).unwrap();
    vault::switch_active(&env, &lock, &path, false).unwrap();
    path
}
fn state(manager: &mut Manager, session: &mut Session) -> Value {
    let binding = manager.handle(session, &json!({"type":"page_state","requestId":"state"}));
    assert_eq!(binding["ok"], true, "{binding}");
    binding
}
fn fixture() -> Fixture {
    let tmp = TempDir::new();
    activate(&tmp, "a.sqlite3");
    let mut session = Session::new(tmp.env());
    let mut manager = Manager::default();
    let binding = state(&mut manager, &mut session);
    let out = tmp.0.join("out");
    fs::create_dir(&out).unwrap();
    Fixture {
        tmp,
        session,
        manager,
        binding,
        out,
    }
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn seed(session: &mut Session, url: &str, title: &str, time: i64, bytes: &[u8]) -> Saved {
    let save_id = uuid::Uuid::new_v4().to_string();
    let hash = sha(bytes);
    let mut id = 0;
    session
        .write(|db| {
            let meta = pages::Metadata {
                url: url.into(),
                title: title.into(),
                captured_at: time,
                incognito: false,
            };
            id = pages::insert(db, &save_id, &meta, bytes, &hash, time)
                .unwrap()
                .saved_page_id;
            Ok(0)
        })
        .unwrap();
    Saved {
        id,
        save_id,
        sha: hash,
        bytes: bytes.to_vec(),
    }
}
/// MHTML-like bytes that any text handling would damage: CRLF, bare CR/LF, NUL, 0xFF, BOM.
fn original(seed: u8, len: usize) -> Vec<u8> {
    let mut bytes =
        b"MIME-Version: 1.0\r\nContent-Type: multipart/related\r\n\r\n\xef\xbb\xbf\r\n\n\r\0\xff"
            .to_vec();
    bytes.extend((0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    bytes
}
fn request(binding: &Value, saved: &Saved) -> Value {
    json!({"type":"page_export","requestId":"export","vaultToken":binding["vaultToken"],
        "generation":binding["generation"],"id":saved.id.to_string(),"saveId":saved.save_id,"sha256":saved.sha})
}
impl Fixture {
    fn export_with(&mut self, saved: &Saved, pick: impl Fn(&str) -> Option<PathBuf>) -> Value {
        let message = request(&self.binding, saved);
        self.manager
            .export(&mut self.session, &Pick(pick), &|| true, &message)
    }
    fn export_to(&mut self, saved: &Saved, path: &Path) -> Value {
        let path = path.to_owned();
        self.export_with(saved, move |_| Some(path.clone()))
    }
    fn count(&mut self, table: &str) -> i64 {
        self.session
            .read(|db| {
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap()
            })
            .unwrap()
    }
    /// Names in the output folder, sorted. Any temporary or set-aside file would show up here.
    fn listing(&self) -> Vec<String> {
        listing(&self.out)
    }
}
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn proposed_names_come_from_the_title_only_and_are_safe_on_windows() {
    let url = "https://user:secret@synthetic.test/a?token=dummy#frag";
    let name = |title: &str| file_name(title, url);
    assert_eq!(name("合成ページ: 第1章"), "合成ページ_ 第1章.mhtml");
    assert_eq!(name(""), "memocampus-page.mhtml");
    assert_eq!(name(" .. . "), "memocampus-page.mhtml");
    assert_eq!(
        name(r"..\..\Windows\System32\evil"),
        "_.._Windows_System32_evil.mhtml"
    );
    assert_eq!(name("../../etc/passwd"), "_.._etc_passwd.mhtml");
    assert_eq!(name(r#"a<b>c:d"e/f\g|h?i*j"#), "a_b_c_d_e_f_g_h_i_j.mhtml");
    for reserved in [
        "CON", "nul", "Com1", "LPT9", "aux.txt", "COM¹", "NUL .x", "conin$",
    ] {
        assert_eq!(name(reserved), format!("_{reserved}.mhtml"), "{reserved}");
    }
    assert_eq!(name("COM10"), "COM10.mhtml");
    assert_eq!(name("Console"), "Console.mhtml");
    // Control characters separate words; bidi overrides and zero-width characters vanish, so a
    // name cannot be displayed as another extension ("gpj.exe" tricks).
    assert_eq!(name("a\r\nb\tc\u{0}d\u{7f}e"), "a b c d e.mhtml");
    assert_eq!(name("invoice\u{202e}lmthm.exe"), "invoicelmthm.exe.mhtml");
    assert_eq!(
        name("a\u{200b}b\u{feff}c\u{2066}d\u{2028}e"),
        "abcd e.mhtml"
    );
    // No doubled extension, no trailing dot or space (Windows silently drops those).
    assert_eq!(name("report.mhtml"), "report.mhtml");
    assert_eq!(name("report.MHTML.mht . "), "report.mhtml");
    assert_eq!(name(".mhtml"), "memocampus-page.mhtml");
    assert_eq!(name("trailing. . ."), "trailing.mhtml");
    // Long titles: at most 120 UTF-16 units before the extension, cut on a character boundary.
    let long = name(&"あ".repeat(70_000));
    assert_eq!(long.encode_utf16().count(), 120 + 6);
    let astral = name(&"😀".repeat(500));
    assert_eq!(astral.encode_utf16().count(), 120 + 6);
    assert!(astral.ends_with("😀.mhtml"));
    // An untitled page shows its URL as the title: never a file name (query, credentials).
    for title in [
        url,
        "user:secret@synthetic.test/a?token=dummy#frag",
        " HTTPS://USER:secret@synthetic.test/a?token=dummy#frag ",
        "see https://other.test/?session=dummy",
    ] {
        assert_eq!(name(title), "memocampus-page.mhtml", "{title}");
    }
    for title in [
        "",
        "x",
        "CON",
        "a/b",
        &"é".repeat(300),
        "\u{202e}",
        "é.mhtml",
    ] {
        let name = name(title);
        assert!(super::file::tests_valid_name(&name), "{name}");
        assert!(!name.contains("secret") && !name.contains("token"));
    }
}

#[test]
fn new_file_is_byte_identical_and_nothing_else_changes() {
    let mut f = fixture();
    // Three chunks with a ragged tail, so chunk boundaries are exercised.
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/p?token=dummy",
        "合成 ページ",
        1,
        &original(1, 2 * pages::CHUNK_BYTES + 17),
    );
    f.session.write(|db| db.execute("INSERT INTO visits VALUES ('keep','https://synthetic.test/','keep','2026-11-01T00:00:00.000Z')", [])).unwrap();
    let before = fs::read(f.tmp.0.join("a.sqlite3")).unwrap();
    let target = f.out.join("書き出し.mhtml");
    let proposed = RefCell::new(String::new());
    let reply = {
        let (target, proposed) = (target.clone(), &proposed);
        let message = request(&f.binding, &saved);
        f.manager.export(
            &mut f.session,
            &Pick(move |name: &str| {
                *proposed.borrow_mut() = name.to_owned();
                Some(target.clone())
            }),
            &|| true,
            &message,
        )
    };
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(*proposed.borrow(), "合成 ページ.mhtml");
    assert_eq!(reply["type"], "page_export_result");
    assert_eq!(reply["exported"], true);
    assert_eq!(reply["fileName"], "書き出し.mhtml");
    assert_eq!(reply["id"], saved.id.to_string());
    assert_eq!(reply["sha256"], saved.sha);
    assert_eq!(reply["byteLength"], saved.bytes.len());
    assert_eq!(reply["previousFileLeft"], false);
    let written = fs::read(&target).unwrap();
    assert_eq!(written.len(), saved.bytes.len());
    assert_eq!(sha(&written), saved.sha);
    assert!(written == saved.bytes);
    assert_eq!(f.listing(), ["書き出し.mhtml"]);
    // Only the file name is reported, never the folder; the reply carries no page content.
    let text = reply.to_string();
    assert!(
        !text.contains("out") && !text.contains("MIME-Version") && !text.contains("token=dummy")
    );
    // Exporting is read-only for the Vault: the file is bit-for-bit what it was.
    f.session.reset();
    assert!(fs::read(f.tmp.0.join("a.sqlite3")).unwrap() == before);
    assert_eq!(
        (
            f.count("saved_pages"),
            f.count("page_blobs"),
            f.count("visits")
        ),
        (1, 1, 1)
    );
}

#[test]
fn cancel_and_refused_overwrite_change_nothing_and_do_not_come_back() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(2, 100),
    );
    let existing = f.out.join("keep.mhtml");
    fs::write(&existing, b"user's own file").unwrap();
    // "No" in the OS overwrite prompt keeps the dialog open; leaving it is a cancel.
    let reply = f.export_with(&saved, |_| None);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["cancelled"], true);
    assert!(reply.get("exported").is_none() && reply.get("fileName").is_none());
    assert_eq!(fs::read(&existing).unwrap(), b"user's own file");
    assert_eq!(f.listing(), ["keep.mhtml"]);
    // Nothing is remembered: a later request with another answer is a new export.
    let other = f.out.join("other.mhtml");
    assert_eq!(f.export_to(&saved, &other)["exported"], true);
    assert_eq!(fs::read(&existing).unwrap(), b"user's own file");
    assert_eq!(f.listing(), ["keep.mhtml", "other.mhtml"]);
}

#[test]
fn the_named_snapshot_is_exported_among_several_and_shared_originals() {
    let mut f = fixture();
    let url = "https://synthetic.test/same";
    let first = seed(&mut f.session, url, "v1", 1, &original(1, 3000));
    let second = seed(&mut f.session, url, "v2", 2, &original(2, 4000));
    let shared = seed(
        &mut f.session,
        "https://other.test/",
        "v1 again",
        3,
        &original(1, 3000),
    );
    assert_eq!((f.count("saved_pages"), f.count("page_blobs")), (3, 2));
    for (index, saved) in [&second, &shared, &first].into_iter().enumerate() {
        let target = f.out.join(format!("{index}.mhtml"));
        assert_eq!(f.export_to(saved, &target)["exported"], true);
        assert!(fs::read(&target).unwrap() == saved.bytes);
    }
    // Deleting one reference of the shared original leaves the other exportable.
    let message = json!({"type":"page_delete_preview","requestId":"p","vaultToken":f.binding["vaultToken"],"generation":f.binding["generation"],"target":{"scope":"event","id":first.id.to_string()}});
    let preview = f.manager.handle(&mut f.session, &message);
    assert_eq!(preview["counts"]["blobsKept"], 1, "{preview}");
    // An export between review and confirmation does not disturb the pending deletion.
    assert_eq!(
        f.export_to(&shared, &f.out.join("mid.mhtml"))["exported"],
        true
    );
    let message = json!({"type":"page_delete","requestId":"d","vaultToken":f.binding["vaultToken"],"generation":f.binding["generation"],"reviewId":preview["reviewId"]});
    assert_eq!(f.manager.handle(&mut f.session, &message)["ok"], true);
    let target = f.out.join("after.mhtml");
    assert_eq!(f.export_to(&shared, &target)["exported"], true);
    assert!(fs::read(&target).unwrap() == shared.bytes);
    assert_eq!(
        f.export_to(&first, &f.out.join("gone.mhtml"))["error"],
        "page_not_found"
    );
    assert!(!f.out.join("gone.mhtml").exists());
}

#[test]
fn request_must_name_the_event_by_id_save_id_and_sha() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(3, 10),
    );
    let target = f.out.join("x.mhtml");
    let asked = std::cell::Cell::new(false);
    let mut attempt = |change: &dyn Fn(&mut Value)| {
        let mut message = request(&f.binding, &saved);
        change(&mut message);
        let reply = f.manager.export(
            &mut f.session,
            &Pick(|_: &str| {
                asked.set(true);
                Some(target.clone())
            }),
            &|| true,
            &message,
        );
        reply["error"].as_str().unwrap_or("").to_owned()
    };
    for bad in [
        json!(0),
        json!(-1),
        json!("1 OR 1=1"),
        json!("01"),
        json!(null),
        json!(9007199254740992_i64),
    ] {
        assert_eq!(attempt(&|m| m["id"] = bad.clone()), "invalid_target");
    }
    assert_eq!(
        attempt(&|m| m["saveId"] = json!("not-a-uuid")),
        "invalid_target"
    );
    assert_eq!(attempt(&|m| m["sha256"] = json!("ABC")), "invalid_target");
    assert_eq!(attempt(&|m| m["id"] = json!("999")), "page_not_found");
    assert_eq!(
        attempt(&|m| m["saveId"] = json!(uuid::Uuid::new_v4().to_string())),
        "page_export_changed"
    );
    assert_eq!(
        attempt(&|m| m["sha256"] = json!("0".repeat(64))),
        "page_export_changed"
    );
    assert_eq!(
        attempt(&|m| m["generation"] = json!("stale")),
        "vault_switched"
    );
    assert_eq!(
        attempt(&|m| m["vaultToken"] = json!("0".repeat(64))),
        "vault_switched"
    );
    assert_eq!(
        attempt(&|m| {
            m.as_object_mut().unwrap().remove("requestId");
        }),
        "invalid_request_id"
    );
    // None of these reached the dialog or the disk.
    assert!(!asked.get());
    assert!(listing(&f.out).is_empty());
}

#[cfg(windows)]
#[test]
fn approved_overwrite_replaces_exactly_the_pinned_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(4, pages::CHUNK_BYTES + 5),
    );
    let target = f.out.join("page.mhtml");
    fs::write(&target, b"old content the user agreed to replace").unwrap();
    // While the export runs, the confirmed file cannot be changed, removed, renamed or replaced
    // by anyone else: it is the very file that gets replaced.
    let probe = target.clone();
    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let _hook = Hook::set(move |stage, index| {
        if stage == "stream" && index == 0 {
            assert!(fs::OpenOptions::new().write(true).open(&probe).is_err());
            assert!(fs::remove_file(&probe).is_err());
            assert!(fs::rename(&probe, probe.with_extension("moved")).is_err());
            assert!(fs::rename(
                probe.parent().unwrap(),
                probe.parent().unwrap().with_extension("moved")
            )
            .is_err());
            assert_eq!(
                fs::read(&probe).unwrap(),
                b"old content the user agreed to replace"
            );
        }
        if stage == "finalize" {
            // Fully written and still not at the final path.
            assert_eq!(
                fs::read(&probe).unwrap(),
                b"old content the user agreed to replace"
            );
        }
        log.borrow_mut().push(stage);
        false
    });
    let reply = f.export_to(&saved, &target);
    assert_eq!(reply["exported"], true, "{reply}");
    assert_eq!(reply["previousFileLeft"], false);
    assert!(fs::read(&target).unwrap() == saved.bytes);
    assert_eq!(f.listing(), ["page.mhtml"]);
    assert!(seen
        .borrow()
        .ends_with(&["finalize", "install", "verify", "discard"]));
    // A file another program holds open is not replaced.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&target)
        .unwrap();
    drop(_hook);
    assert_eq!(f.export_to(&saved, &target)["error"], "export_target_busy");
    drop(held);
    // Nor a read-only one, nor a folder of that name.
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions.clone()).unwrap();
    assert_eq!(
        f.export_to(&saved, &target)["error"],
        "export_target_readonly"
    );
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&target, permissions).unwrap();
    fs::create_dir(f.out.join("folder.mhtml")).unwrap();
    assert_eq!(
        f.export_to(&saved, &f.out.join("folder.mhtml"))["error"],
        "export_invalid_target"
    );
    assert!(fs::read(&target).unwrap() == saved.bytes);
    assert_eq!(f.listing(), ["folder.mhtml", "page.mhtml"]);
}

#[cfg(windows)]
#[test]
fn a_file_that_appears_after_the_dialog_is_never_overwritten() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(5, 2000),
    );
    // 1. The name was free when the dialog closed (no overwrite question was asked), then another
    //    process creates it while the original is being written.
    let target = f.out.join("new.mhtml");
    let intruder = target.clone();
    let hook = Hook::set(move |stage, _| {
        if stage == "finalize" {
            fs::write(&intruder, b"someone else's file").unwrap();
        }
        false
    });
    assert_eq!(
        f.export_to(&saved, &target)["error"],
        "export_file_appeared"
    );
    assert_eq!(fs::read(&target).unwrap(), b"someone else's file");
    assert_eq!(f.listing(), ["new.mhtml"]);
    drop(hook);
    // 2. Replacing an approved file: the name is taken in the instant it is free. The approved
    //    file's content survives beside it; the intruder is untouched; no success is reported.
    let target = f.out.join("old.mhtml");
    fs::write(&target, b"approved").unwrap();
    let intruder = target.clone();
    let hook = Hook::set(move |stage, _| {
        if stage == "install" {
            fs::write(&intruder, b"raced in").unwrap();
        }
        false
    });
    assert_eq!(
        f.export_to(&saved, &target)["error"],
        "export_recovery_needed"
    );
    drop(hook);
    assert_eq!(fs::read(&target).unwrap(), b"raced in");
    let names = f.listing();
    assert_eq!(names.len(), 3, "{names:?}");
    let aside = names
        .iter()
        .find(|name| name.starts_with("old.mhtml.") && name.ends_with(".memocampus-old"))
        .unwrap();
    assert_eq!(fs::read(f.out.join(aside)).unwrap(), b"approved");
}

#[cfg(windows)]
#[test]
fn failures_at_every_stage_leave_the_destination_as_it_was() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(6, 3 * pages::CHUNK_BYTES),
    );
    let fresh = f.out.join("fresh.mhtml");
    let kept = f.out.join("kept.mhtml");
    fs::write(&kept, b"kept").unwrap();
    for (stage, at, error) in [
        ("temp", 0, "export_failed"),
        ("stream", 0, "export_failed"),
        ("stream", 2, "export_failed"),
        ("finalize", 0, "export_failed"),
        ("install", 0, "export_failed"),
        ("verify", 0, "export_verify_failed"),
    ] {
        for target in [&fresh, &kept] {
            let hook = Hook::set(move |now, index| now == stage && index == at);
            let reply = f.export_to(&saved, target);
            drop(hook);
            assert_eq!(reply["error"], error, "{stage} {at}");
            assert!(reply.get("exported").is_none() && reply.get("fileName").is_none());
            assert_eq!(fs::read(&kept).unwrap(), b"kept", "{stage} {at}");
            assert_eq!(f.listing(), ["kept.mhtml"], "{stage} {at}");
        }
    }
    // The replaced file could not be removed after a verified install: success, and said so.
    let hook = Hook::set(|stage, _| stage == "discard");
    let reply = f.export_to(&saved, &kept);
    drop(hook);
    assert_eq!(
        (&reply["exported"], &reply["previousFileLeft"]),
        (&json!(true), &json!(true))
    );
    assert!(fs::read(&kept).unwrap() == saved.bytes);
    assert_eq!(f.listing().len(), 2);
    assert_eq!((f.count("saved_pages"), f.count("page_blobs")), (1, 1));
}

#[cfg(windows)]
#[test]
fn unusable_destinations_are_refused_before_anything_is_written() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(7, 10),
    );
    let out = f.out.clone();
    for (path, error) in [
        (out.join("missing").join("x.mhtml"), "parent_missing"),
        (PathBuf::from("relative.mhtml"), "export_invalid_target"),
        (PathBuf::from(r"C:\"), "export_invalid_target"),
        (out.join("NUL.mhtml"), "export_invalid_target"),
        (out.join("com1"), "export_invalid_target"),
        (out.join("dot.mhtml."), "export_invalid_target"),
        (out.join("space.mhtml "), "export_invalid_target"),
        (out.join("a:stream.mhtml"), "export_invalid_target"),
        (out.join(".."), "export_invalid_target"),
    ] {
        assert_eq!(f.export_to(&saved, &path)["error"], error, "{path:?}");
    }
    let file = out.join("file");
    fs::write(&file, b"f").unwrap();
    assert_eq!(
        f.export_to(&saved, &file.join("x.mhtml"))["error"],
        "parent_missing"
    );
    assert_eq!(f.listing(), ["file"]);
    assert!(!Path::new("relative.mhtml").exists());
}

#[cfg(windows)]
#[test]
fn junction_retargeting_and_link_targets_cannot_redirect_the_export() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(8, 5000),
    );
    let (real, other, link) = (f.out.join("real"), f.out.join("other"), f.out.join("link"));
    fs::create_dir(&real).unwrap();
    fs::create_dir(&other).unwrap();
    fn junction(link: &Path, to: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(to)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
    }
    junction(&link, &real);
    // The user picked a file through the junction. While the original is written, the junction
    // is re-pointed at another folder: the file still lands in the folder that was chosen.
    let (relink, to) = (link.clone(), other.clone());
    let hook = Hook::set(move |stage, index| {
        if stage == "stream" && index == 0 {
            fs::remove_dir(&relink).unwrap();
            junction(&relink, &to);
        }
        false
    });
    let reply = f.export_to(&saved, &link.join("page.mhtml"));
    drop(hook);
    assert_eq!(reply["exported"], true, "{reply}");
    assert!(fs::read(real.join("page.mhtml")).unwrap() == saved.bytes);
    assert!(listing(&other).is_empty());
    assert_eq!(listing(&real), ["page.mhtml"]);
    // An existing name that is itself a link is refused: its target is not what the user saw.
    let victim = other.join("victim.txt");
    fs::write(&victim, b"victim").unwrap();
    let alias = real.join("alias.mhtml");
    match std::os::windows::fs::symlink_file(&victim, &alias) {
        Ok(()) => {
            assert_eq!(
                f.export_to(&saved, &alias)["error"],
                "export_invalid_target"
            );
            assert_eq!(fs::read(&victim).unwrap(), b"victim");
        }
        Err(error) => eprintln!("symlink case skipped (no privilege to create one): {error}"),
    }
    // A hard link to another file: only the chosen name changes, the other name keeps its data.
    let twin = real.join("twin.mhtml");
    fs::hard_link(&victim, &twin).unwrap();
    assert_eq!(f.export_to(&saved, &twin)["exported"], true);
    assert_eq!(fs::read(&victim).unwrap(), b"victim");
    assert!(fs::read(&twin).unwrap() == saved.bytes);
}

#[test]
fn vault_a_b_a_while_the_dialog_is_open_stops_the_export() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(9, 100),
    );
    // Vault B holds a different page under the same event id.
    activate(&f.tmp, "b.sqlite3");
    let decoy = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(10, 100),
    );
    assert_eq!(decoy.id, saved.id);
    activate(&f.tmp, "a.sqlite3");
    f.binding = state(&mut f.manager, &mut f.session);
    let target = f.out.join("x.mhtml");
    fs::write(&target, b"approved for replacement").unwrap();
    // A → B → A while the dialog is open: same file, same token, new selection generation.
    let reply = {
        let tmp = &f.tmp;
        let chosen = target.clone();
        let message = request(&f.binding, &saved);
        f.manager.export(
            &mut f.session,
            &Pick(move |_: &str| {
                activate(tmp, "b.sqlite3");
                activate(tmp, "a.sqlite3");
                Some(chosen.clone())
            }),
            &|| true,
            &message,
        )
    };
    assert_eq!(reply["error"], "vault_switched", "{reply}");
    assert_eq!(fs::read(&target).unwrap(), b"approved for replacement");
    assert_eq!(f.listing(), ["x.mhtml"]);
    // Left on B: the same request never exports B's page with the same event id.
    let reply = {
        let tmp = &f.tmp;
        let chosen = target.clone();
        let message = request(&state(&mut f.manager, &mut f.session), &saved);
        f.manager.export(
            &mut f.session,
            &Pick(move |_: &str| {
                activate(tmp, "b.sqlite3");
                Some(chosen.clone())
            }),
            &|| true,
            &message,
        )
    };
    assert_eq!(reply["error"], "vault_switched");
    let message = request(&state(&mut f.manager, &mut f.session), &saved);
    let chosen = target.clone();
    let reply = f.manager.export(
        &mut f.session,
        &Pick(move |_: &str| Some(chosen.clone())),
        &|| true,
        &message,
    );
    assert_eq!(reply["error"], "page_export_changed");
    assert_eq!(fs::read(&target).unwrap(), b"approved for replacement");
    // The old response cannot be mistaken for the new Vault's: the UI's stale request is refused
    // by the Host as well.
    let reply = f.export_to(&saved, &target);
    assert_eq!(reply["error"], "vault_switched");
}

#[test]
fn selection_is_locked_during_the_write_and_rechecked_before_the_final_path_changes() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(11, 2 * pages::CHUNK_BYTES),
    );
    let target = f.out.join("x.mhtml");
    let env = f.tmp.env();
    // A cooperating process (any Memocampus host) cannot change the selection mid-export.
    let hook = Hook::set(move |stage, index| {
        if stage == "stream" && index == 1 {
            assert!(ConfigLock::acquire_within(&env, Duration::ZERO).is_err());
        }
        false
    });
    assert_eq!(f.export_to(&saved, &target)["exported"], true);
    drop(hook);
    fs::remove_file(&target).unwrap();
    // Even a writer that ignores the lock is noticed before the final path is touched.
    let env = f.tmp.env();
    let hook = Hook::set(move |stage, index| {
        if stage == "stream" && index == 1 {
            let vault::Active::Configured(mut config) = vault::resolve(&env).unwrap() else {
                panic!("configured")
            };
            config.selection_generation = Some(uuid::Uuid::new_v4().to_string());
            vault::write_config(&env, &config).unwrap();
        }
        false
    });
    assert_eq!(f.export_to(&saved, &target)["error"], "vault_switched");
    drop(hook);
    assert!(f.listing().is_empty());
}

#[test]
fn deleted_replaced_or_added_events_during_the_dialog() {
    let mut f = fixture();
    let url = "https://synthetic.test/same";
    let saved = seed(&mut f.session, url, "t", 1, &original(12, 100));
    let target = f.out.join("x.mhtml");
    // A newer save of the same URL (and of the same original) does not change what is exported.
    let reply = {
        let tmp = &f.tmp;
        let chosen = target.clone();
        let message = request(&f.binding, &saved);
        f.manager.export(
            &mut f.session,
            &Pick(move |_: &str| {
                let mut other = Session::new(tmp.env());
                seed(&mut other, url, "newer", 2, &original(13, 100));
                seed(&mut other, url, "same original", 3, &original(12, 100));
                Some(chosen.clone())
            }),
            &|| true,
            &message,
        )
    };
    assert_eq!(reply["exported"], true, "{reply}");
    assert!(fs::read(&target).unwrap() == saved.bytes);
    fs::write(&target, b"approved").unwrap();
    // Deleted while the dialog is open, and its row id reused by another save: stop.
    let last = seed(
        &mut f.session,
        "https://synthetic.test/last",
        "last",
        4,
        &original(14, 100),
    );
    let reply = {
        let tmp = &f.tmp;
        let chosen = target.clone();
        let id = last.id;
        let message = request(&f.binding, &last);
        f.manager.export(
            &mut f.session,
            &Pick(move |_: &str| {
                let mut other = Session::new(tmp.env());
                other
                    .write(|db| db.execute("DELETE FROM saved_pages WHERE id=?1", [id]))
                    .unwrap();
                assert_eq!(
                    seed(
                        &mut other,
                        "https://synthetic.test/reuse",
                        "reused",
                        5,
                        &original(15, 100)
                    )
                    .id,
                    id
                );
                Some(chosen.clone())
            }),
            &|| true,
            &message,
        )
    };
    assert_eq!(reply["error"], "page_export_changed", "{reply}");
    let reply = {
        let tmp = &f.tmp;
        let chosen = target.clone();
        let id = saved.id;
        let message = request(&f.binding, &saved);
        f.manager.export(
            &mut f.session,
            &Pick(move |_: &str| {
                let mut other = Session::new(tmp.env());
                other
                    .write(|db| db.execute("DELETE FROM saved_pages WHERE id=?1", [id]))
                    .unwrap();
                Some(chosen.clone())
            }),
            &|| true,
            &message,
        )
    };
    assert_eq!(reply["error"], "page_not_found", "{reply}");
    assert_eq!(fs::read(&target).unwrap(), b"approved");
    assert_eq!(f.listing(), ["x.mhtml"]);
}

#[test]
fn a_deletion_cannot_interleave_with_the_write() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(16, 2 * pages::CHUNK_BYTES),
    );
    let target = f.out.join("x.mhtml");
    let vault = f.tmp.0.join("a.sqlite3");
    let id = saved.id;
    let refused = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = refused.clone();
    let hook = Hook::set(move |stage, index| {
        if stage == "stream" && index == 1 {
            // Another host's deletion, arriving while the original is read: the export holds one
            // read snapshot, so the writer waits (here: gives up at once) instead of interleaving.
            let other = rusqlite::Connection::open(&vault).unwrap();
            other.busy_timeout(Duration::ZERO).unwrap();
            seen.set(
                other
                    .execute("DELETE FROM saved_pages WHERE id=?1", params![id])
                    .is_err(),
            );
        }
        false
    });
    let reply = f.export_to(&saved, &target);
    drop(hook);
    assert_eq!(reply["exported"], true, "{reply}");
    assert!(refused.get());
    assert!(fs::read(&target).unwrap() == saved.bytes);
    assert_eq!(f.count("saved_pages"), 1);
}

#[test]
fn damaged_originals_are_never_written_out() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(17, pages::CHUNK_BYTES + 3),
    );
    let target = f.out.join("x.mhtml");
    fs::write(&target, b"approved").unwrap();
    // Same length, one flipped byte in the last chunk: the SHA-256 no longer matches.
    f.session
        .write(|db| db.execute("UPDATE page_blobs SET data = CAST(substr(data, 1, length(data) - 1) || x'00' AS BLOB)", []))
        .unwrap();
    assert_eq!(f.export_to(&saved, &target)["error"], "page_blob_corrupt");
    assert_eq!(fs::read(&target).unwrap(), b"approved");
    assert_eq!(f.listing(), ["x.mhtml"]);
    // A zero-length original cannot be stored (schema CHECK and `pages::insert`), so there is
    // nothing to export; the smallest original is one byte.
    assert_eq!(pages::validate_size(0), Err("invalid_page_size"));
    assert!(f
        .session
        .write(|db| db.execute("UPDATE page_blobs SET byte_length = 0, data = x''", []))
        .is_err());
    let one = seed(&mut f.session, "https://synthetic.test/1", "one", 2, b"\n");
    let small = f.out.join("one.mhtml");
    assert_eq!(f.export_to(&one, &small)["exported"], true);
    assert_eq!(fs::read(&small).unwrap(), b"\n");
}

#[test]
fn a_closed_port_after_the_dialog_writes_nothing() {
    let mut f = fixture();
    let saved = seed(
        &mut f.session,
        "https://synthetic.test/",
        "t",
        1,
        &original(18, 100),
    );
    let target = f.out.join("x.mhtml");
    fs::write(&target, b"approved").unwrap();
    let message = request(&f.binding, &saved);
    let chosen = target.clone();
    let reply = f.manager.export(
        &mut f.session,
        &Pick(move |_: &str| Some(chosen.clone())),
        &|| false,
        &message,
    );
    assert_eq!(reply["error"], "host_unavailable");
    assert_eq!(fs::read(&target).unwrap(), b"approved");
    assert_eq!(f.listing(), ["x.mhtml"]);
    // A restarted Host (new Manager, nothing remembered) does not resume anything by itself and
    // serves a new, explicit request normally.
    let mut restarted = Manager::default();
    let binding = state(&mut restarted, &mut f.session);
    let chosen = target.clone();
    let reply = restarted.export(
        &mut f.session,
        &Pick(move |_: &str| Some(chosen.clone())),
        &|| true,
        &request(&binding, &saved),
    );
    assert_eq!(reply["exported"], true, "{reply}");
    assert!(fs::read(&target).unwrap() == saved.bytes);
}

#[test]
fn locked_secure_vault_stops_the_export_before_and_after_the_dialog() {
    use memocampus_native_host::secure_vault::{
        cipher,
        secret::{CipherSalt, VaultId, VaultKey},
    };
    use std::{cell::Cell, rc::Rc};
    struct Keys(Rc<Cell<bool>>);
    impl vault::KeySource for Keys {
        fn key_for(&self, _: &vault::KeyRequest) -> Result<VaultKey, vault::KeyError> {
            if self.0.get() {
                Ok(VaultKey::from_bytes(&mut [7; 32]))
            } else {
                Err(vault::KeyError::Locked)
            }
        }
    }
    let tmp = TempDir::new();
    let env = tmp.env();
    let path = tmp.0.join("secure.sqlite3");
    cipher::create(
        &path,
        &VaultKey::from_bytes(&mut [7; 32]),
        &CipherSalt::from_array([2; 16]),
    )
    .unwrap();
    let config = vault::Config::encrypted(
        path.to_string_lossy().into_owned(),
        false,
        VaultId::from_array([1; 16]),
    );
    vault::write_config(&env, &config).unwrap();
    let unlocked = Rc::new(Cell::new(true));
    let mut session = Session::with_keys(env, Box::new(Keys(unlocked.clone())));
    let mut manager = Manager::default();
    let binding = state(&mut manager, &mut session);
    let saved = seed(
        &mut session,
        "https://synthetic.test/",
        "secure",
        1,
        &original(19, pages::CHUNK_BYTES + 1),
    );
    let target = tmp.0.join("x.mhtml");
    // An encrypted Vault's original comes out as the plaintext MHTML that was saved.
    let chosen = target.clone();
    let reply = manager.export(
        &mut session,
        &Pick(move |_: &str| Some(chosen.clone())),
        &|| true,
        &request(&binding, &saved),
    );
    assert_eq!(reply["exported"], true, "{reply}");
    assert!(fs::read(&target).unwrap() == saved.bytes);
    fs::write(&target, b"approved").unwrap();
    // Locked while the dialog is open.
    let (chosen, key) = (target.clone(), unlocked.clone());
    let reply = manager.export(
        &mut session,
        &Pick(move |_: &str| {
            key.set(false);
            Some(chosen.clone())
        }),
        &|| true,
        &request(&binding, &saved),
    );
    assert_eq!(reply["error"], "vault_locked", "{reply}");
    // Locked before: the dialog is not even shown.
    let asked = Cell::new(false);
    let reply = manager.export(
        &mut session,
        &Pick(|_: &str| {
            asked.set(true);
            None
        }),
        &|| true,
        &request(&binding, &saved),
    );
    assert_eq!(reply["error"], "vault_locked");
    assert!(!asked.get());
    assert_eq!(fs::read(&target).unwrap(), b"approved");
    assert_eq!(
        listing(&tmp.0)
            .iter()
            .filter(|name| name.contains("memocampus-"))
            .count(),
        0
    );
}

/// 128 MiB, the largest original a Vault can hold. The original is seeded and compared in
/// chunks, so this test's own peak memory is the export's (see scripts/measure-page-export.ps1).
#[test]
fn exact_128_mib_original_streams_to_an_identical_file() {
    use std::io::{Read, Write};
    let mut f = fixture();
    let chunk: Vec<u8> = (0..pages::CHUNK_BYTES).map(|i| (i % 251) as u8).collect();
    let chunks = pages::MAX_PAGE_BYTES / pages::CHUNK_BYTES;
    let mut hash = Sha256::new();
    for index in 0..chunks {
        hash.update([index as u8]);
        hash.update(&chunk[1..]);
    }
    let hash = format!("{:x}", hash.finalize());
    let save_id = uuid::Uuid::new_v4().to_string();
    let mut id = 0;
    f.session
        .write(|db| {
            db.execute("INSERT INTO page_blobs (sha256, byte_length, format, mime_type, data) VALUES (?1, ?2, 'mhtml', 'multipart/related', zeroblob(?2))", params![hash, pages::MAX_PAGE_BYTES as i64])?;
            let blob_id = db.last_insert_rowid();
            let mut blob = db.blob_open(rusqlite::DatabaseName::Main, "page_blobs", "data", blob_id, false)?;
            for index in 0..chunks {
                blob.write_all(&[index as u8]).unwrap();
                blob.write_all(&chunk[1..]).unwrap();
            }
            drop(blob);
            db.execute("INSERT INTO saved_pages (save_id, blob_id, url, title, captured_at, saved_at, incognito) VALUES (?1, ?2, 'https://synthetic.test/large', 'large', 1, 1, 0)", params![save_id, blob_id])?;
            id = db.last_insert_rowid();
            Ok(0)
        })
        .unwrap();
    let saved = Saved {
        id,
        save_id,
        sha: hash.clone(),
        bytes: Vec::new(),
    };
    let target = f.out.join("large.mhtml");
    let started = std::time::Instant::now();
    let reply = f.export_to(&saved, &target);
    let elapsed = started.elapsed();
    assert_eq!(reply["exported"], true, "{reply}");
    assert_eq!(reply["byteLength"], pages::MAX_PAGE_BYTES);
    assert_eq!(
        fs::metadata(&target).unwrap().len(),
        pages::MAX_PAGE_BYTES as u64
    );
    let mut file = fs::File::open(&target).unwrap();
    let mut buffer = vec![0u8; pages::CHUNK_BYTES];
    let mut check = Sha256::new();
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        check.update(&buffer[..n]);
    }
    assert_eq!(format!("{:x}", check.finalize()), hash);
    assert_eq!(f.listing(), ["large.mhtml"]);
    eprintln!(
        "synthetic Page 128MiB export (stream + sync + verify) {elapsed:?}; block={} bytes",
        pages::CHUNK_BYTES
    );
}
