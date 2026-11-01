//! In its own test binary because it changes process environment variables. Those point at a
//! temporary directory, so the user's real application directory is never read.

use memocampus_native_host::policy_store::PolicyStore;
use memocampus_visible_store::{SearchVisits, VisibleError, VisibleStore};
use rusqlite::Connection;

#[test]
fn the_policy_comes_from_the_local_app_dir_independently_of_the_vault() {
    let dir = std::env::temp_dir().join(format!("memocampus-process-{}", uuid::Uuid::new_v4()));
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::env::remove_var("MEMOCAMPUS_APP_DIR");
    std::env::set_var("LOCALAPPDATA", &dir);

    let vault = elsewhere.join("v.sqlite3");
    Connection::open(&vault)
        .unwrap()
        .execute_batch(&format!(
            "{} INSERT INTO visits VALUES ('1', 'https://public.test/', 'P', '2026-10-01T00:00:00.000Z');",
            memocampus_native_host::schema::create_sql()
        ))
        .unwrap();

    let store = VisibleStore::from_process(vault).unwrap();
    assert_eq!(
        store.search_visits(&SearchVisits::default()).err(),
        Some(VisibleError::PolicyUnavailable),
        "nothing was initialized under LOCALAPPDATA yet"
    );
    assert!(!dir.join("Memocampus").exists(), "a read creates nothing");

    PolicyStore::new(dir.join("Memocampus"))
        .unwrap()
        .initialize()
        .unwrap();
    assert_eq!(
        store.search_visits(&SearchVisits::default()).unwrap().total,
        1
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
