//! Crash recovery is exercised in another process, not just another thread.
use memocampus_native_host::policy_store::PolicyStore;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn policy_writer_child() {
    let Some(dir) = std::env::var_os("MEMOCAMPUS_POLICY_CRASH_TEST_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("policy.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("policy.crashed.tmp"))
        .unwrap();
    temp.write_all(b"{interrupted").unwrap();
    temp.sync_all().unwrap();
    fs::write(dir.join("child-ready"), b"ready").unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn killed_writer_leaves_old_policy_and_releases_os_lock() {
    let dir =
        std::env::temp_dir().join(format!("memocampus-policy-crash-{}", uuid::Uuid::new_v4()));
    let store = PolicyStore::new(dir.clone()).unwrap();
    let first = store.initialize().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "policy_writer_child", "--nocapture"])
        .env("MEMOCAMPUS_POLICY_CRASH_TEST_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.join("child-ready").exists() && Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let ready = dir.join("child-ready").exists();
    let contention = store.update(&first, first.policy.clone());
    let _ = child.kill();
    child.wait().unwrap();
    assert!(ready, "child must hold the lock before termination");
    assert_eq!(
        contention.unwrap_err(),
        memocampus_native_host::policy_store::StoreError::Busy
    );
    assert_eq!(store.load().unwrap().etag, first.etag);
    assert_eq!(
        store
            .update(&first, first.policy.clone())
            .unwrap()
            .policy
            .revision,
        2
    );
    assert_eq!(
        fs::read(dir.join("policy.crashed.tmp")).unwrap(),
        b"{interrupted"
    );
    fs::remove_dir_all(dir).unwrap();
}
