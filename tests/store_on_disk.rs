// Regression test for ISSUE-002: antenna panics on startup with TryFromIntError.
// Opens a fresh disk-backed oxigraph store and round-trips an INSERT/ASK.
// Before the fd_limit fix this test panicked on macOS with "unlimited" ulimit.

use antenna::store::RdfStore;

#[test]
fn disk_store_opens_without_panic() {
    let dir = std::env::temp_dir().join(format!(
        "antenna-issue002-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let store = RdfStore::open(Some(dir.to_str().unwrap()))
        .expect("disk store must open without panicking on TryFromIntError");
    store.insert_turtle("<urn:s> a <urn:T> .").unwrap();
    assert!(store.ask("ASK { <urn:s> a <urn:T> }").unwrap());

    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
