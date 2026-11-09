fn main() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let v: String = db.query_row("SELECT sqlite_version()", [], |r| r.get(0)).unwrap();
    let c: Option<String> = db.query_row("PRAGMA cipher_version", [], |r| r.get(0)).ok();
    println!("sqlite_version={v} cipher_version={c:?}");
}
