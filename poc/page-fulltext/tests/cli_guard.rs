//! Review fixes MEDIUM-1 and MEDIUM-2 through the `page-index` CLI (report XIV-13): no
//! plaintext database is ever created inside a Git working tree, and the read-only commands
//! create nothing.
//!
//! The "repositories" are directories with a `.git` entry under the system temporary directory
//! (outside this repository), plus this repository itself for the real case. Inputs are the
//! Chrome fixtures (synthetic text).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_page-index");

fn base() -> PathBuf {
    let d = std::env::temp_dir().join(format!("page-index-guard-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chrome/ja-article.mhtml")
}

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn add(cwd: &Path, db: &str) -> Output {
    run_in(
        cwd,
        &[
            "add",
            "--db",
            db,
            "--key",
            "k",
            "--input",
            fixture().to_str().unwrap(),
        ],
    )
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[track_caller]
fn assert_refused(o: &Output, created: &Path) {
    assert!(!o.status.success(), "accepted: {}", stderr(o));
    assert!(stderr(o).contains("refused"), "{}", stderr(o));
    assert!(!created.exists(), "{} was created", created.display());
}

/// base/<name> with a `.git` directory (or a `.git` file, as in a linked worktree).
fn fake_repo(name: &str, git_file: bool) -> PathBuf {
    let r = base().join(name);
    std::fs::create_dir_all(r.join("sub")).unwrap();
    if git_file {
        std::fs::write(r.join(".git"), "gitdir: elsewhere\n").unwrap();
    } else {
        std::fs::create_dir_all(r.join(".git")).unwrap();
    }
    r
}

#[test]
fn c_outside_a_repository_is_allowed() {
    let out = base().join("outside");
    std::fs::create_dir_all(&out).unwrap();
    let db = out.join("ok.sqlite3");
    let o = add(&out, db.to_str().unwrap());
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(db.exists());
    // A relative path, resolved against the working directory.
    let o = add(&out, "relative.sqlite3");
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(out.join("relative.sqlite3").exists());
    // The read-only commands on the database just made.
    let o = run_in(&out, &["search", "--db", "ok.sqlite3", "--query", "日本"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = run_in(&out, &["stats", "--db", "ok.sqlite3"]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn c_inside_a_repository_is_refused() {
    let repo = fake_repo("repo", false);
    // Repository root, an existing subdirectory, a file in directories that do not exist yet,
    // and the .git directory itself.
    for db in [
        repo.join("root.sqlite3"),
        repo.join("sub").join("sub.sqlite3"),
        repo.join("new").join("deeper").join("x.sqlite3"),
        repo.join(".git").join("in-git-dir.sqlite3"),
    ] {
        assert_refused(&add(&base(), db.to_str().unwrap()), &db);
    }
    assert!(!repo.join("new").exists());
    // A relative path from inside the repository.
    let o = add(&repo.join("sub"), "relative.sqlite3");
    assert_refused(&o, &repo.join("sub").join("relative.sqlite3"));
    let o = add(&repo.join("sub"), "../up.sqlite3");
    assert_refused(&o, &repo.join("up.sqlite3"));
    // A linked worktree or submodule (`.git` is a file).
    let wt = fake_repo("worktree", true);
    let db = wt.join("sub").join("w.sqlite3");
    assert_refused(&add(&base(), db.to_str().unwrap()), &db);
    // Every database command is guarded, the read-only ones included.
    for cmd in ["search", "stats", "delete", "rebuild", "check"] {
        let db = repo.join(format!("{cmd}.sqlite3"));
        let mut args = vec![cmd, "--db", db.to_str().unwrap()];
        if cmd == "search" {
            args.extend(["--query", "x"]);
        }
        if cmd == "delete" {
            args.extend(["--key", "k"]);
        }
        assert_refused(&run_in(&base(), &args), &db);
    }
    // The explicit override still works.
    let db = repo.join("allowed.sqlite3");
    let o = run_in(
        &base(),
        &[
            "add",
            "--db",
            db.to_str().unwrap(),
            "--key",
            "k",
            "--input",
            fixture().to_str().unwrap(),
            "--allow-db-in-git",
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn c_this_repository_is_refused() {
    // The real working tree of this repository (target/ is ignored by Git but still inside it).
    let db = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("guard-{}.sqlite3", std::process::id()));
    assert_refused(&add(&base(), db.to_str().unwrap()), &db);
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) -> bool {
    let _ = std::fs::remove_dir(link);
    Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(windows)]
#[test]
fn c_windows_junction_into_a_repository_is_refused() {
    let repo = fake_repo("junction-target", false);
    // The junction points at a subdirectory, so only the real path has the `.git` ancestor.
    let link = base().join("junction");
    assert!(junction(&link, &repo.join("sub")), "mklink /J failed");
    assert!(!link.join(".git").exists());
    let db = link.join("j.sqlite3");
    assert_refused(&add(&base(), db.to_str().unwrap()), &db);
    assert!(!repo.join("sub").join("j.sqlite3").exists());
    // Through the junction, in a directory that does not exist yet.
    let db = link.join("new").join("j.sqlite3");
    assert_refused(&add(&base(), db.to_str().unwrap()), &db);
    // bench --dir through the junction.
    let d = link.join("bench");
    let o = run_in(
        &base(),
        &["bench", "--dir", d.to_str().unwrap(), "--pages", "10"],
    );
    assert_refused(&o, &d);
}

#[test]
fn c_symbolic_link_into_a_repository_is_refused() {
    let repo = fake_repo("symlink-target", false);
    let link = base().join("symlink");
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_file(&link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(repo.join("sub"), &link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(repo.join("sub"), &link);
    if let Err(e) = made {
        // Windows without Developer Mode or the privilege: reported, the junction test covers
        // the same resolution.
        eprintln!("symbolic link not available here ({e}); skipped");
        return;
    }
    let db = link.join("s.sqlite3");
    assert_refused(&add(&base(), db.to_str().unwrap()), &db);
    // A symbolic link to a database file inside the repository.
    let target = repo.join("sub").join("target.sqlite3");
    let file_link = base().join("file-link.sqlite3");
    let _ = std::fs::remove_file(&file_link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&target, &file_link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &file_link);
    made.unwrap();
    assert_refused(&add(&base(), file_link.to_str().unwrap()), &target);
}

#[test]
fn c_bench_dir_is_guarded_and_never_deletes() {
    let repo = fake_repo("bench-repo", false);
    for d in [repo.join("bench"), repo.clone(), repo.join("sub")] {
        let o = run_in(
            &base(),
            &["bench", "--dir", d.to_str().unwrap(), "--pages", "10"],
        );
        assert!(!o.status.success());
        assert!(stderr(&o).contains("refused"), "{}", stderr(&o));
    }
    assert!(!repo.join("bench").exists());
    // Outside: runs (tiny), and a second run into the same directory refuses instead of
    // deleting the existing database.
    let d = base().join("bench-out");
    let _ = std::fs::remove_dir_all(&d);
    let args = [
        "bench",
        "--dir",
        d.to_str().unwrap(),
        "--pages",
        "20",
        "--runs",
        "1",
    ];
    let o = run_in(&base(), &args);
    assert!(o.status.success(), "{}", stderr(&o));
    let db = d.join("bench-20-none.sqlite3");
    let before = std::fs::read(&db).unwrap();
    let o = run_in(&base(), &args);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("exists"), "{}", stderr(&o));
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

#[test]
fn c_read_only_commands_create_nothing() {
    let out = base().join("readonly");
    std::fs::create_dir_all(&out).unwrap();
    // Missing database: an error, no file.
    for cmd in [
        &["search", "--db", "missing.sqlite3", "--query", "x"][..],
        &["stats", "--db", "missing.sqlite3"],
        &["check", "--db", "missing.sqlite3"],
        &["rebuild", "--db", "missing.sqlite3"],
        &["delete", "--db", "missing.sqlite3", "--key", "k"],
    ] {
        let o = run_in(&out, cmd);
        assert!(!o.status.success());
        assert!(stderr(&o).contains("not_found"), "{}", stderr(&o));
        assert!(!out.join("missing.sqlite3").exists());
    }
    // Empty file: not initialized by any command.
    std::fs::write(out.join("empty.sqlite3"), b"").unwrap();
    for cmd in [
        &["search", "--db", "empty.sqlite3", "--query", "x"][..],
        &["stats", "--db", "empty.sqlite3"],
        &[
            "add",
            "--db",
            "empty.sqlite3",
            "--key",
            "k",
            "--input",
            fixture().to_str().unwrap(),
        ],
    ] {
        let o = run_in(&out, cmd);
        assert!(!o.status.success());
        assert!(stderr(&o).contains("not_a_page_index"), "{}", stderr(&o));
        assert_eq!(
            std::fs::metadata(out.join("empty.sqlite3")).unwrap().len(),
            0
        );
    }
    // A valid database: search and stats change neither it nor its side files.
    let o = add(&out, "valid.sqlite3");
    assert!(o.status.success(), "{}", stderr(&o));
    let snap = || {
        ["", "-wal", "-shm", "-journal"]
            .map(|s| std::fs::read(out.join(format!("valid.sqlite3{s}"))).ok())
    };
    let before = snap();
    assert!(before[1..].iter().all(Option::is_none));
    for cmd in [
        &["search", "--db", "valid.sqlite3", "--query", "日本"][..],
        &["search", "--db", "valid.sqlite3", "--query", "日本語の"],
        &["stats", "--db", "valid.sqlite3"],
    ] {
        let o = run_in(&out, cmd);
        assert!(o.status.success(), "{}", stderr(&o));
        assert_eq!(snap(), before);
    }
}
