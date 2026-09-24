//! aegis-uqu3pr: a write-lock timeout names the process holding the lock.
//!
//! Lives here rather than beside the code because the sync-authority source
//! scan (validation::tests::sync_safety_source_scan_accepts_complete_real_tree)
//! reads all of src/sync/mod.rs, tests included, and forbids process-spawning
//! markers there. Spawning a real holder is the point of this test.
#![cfg(target_os = "linux")]

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use beads_rust::sync::blocking_write_lock_with_timeout;

#[test]
fn a_contended_lock_names_the_process_holding_it() {
    let Some(flock) = ["/usr/bin/flock", "/bin/flock"]
        .iter()
        .find(|path| std::path::Path::new(path).is_file())
    else {
        return; // util-linux `flock` absent: nothing to contend with
    };
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join(".write.lock");
    fs::write(&lock, b"").unwrap();
    let mut holder = Command::new(flock)
        .args(["-x", lock.to_str().unwrap(), "sleep", "30"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    // Wait until the holder really has the lock: our own short attempt fails.
    let deadline = Instant::now() + Duration::from_secs(5);
    let error = loop {
        match blocking_write_lock_with_timeout(dir.path(), Some(100)) {
            Err(error) => break error.to_string(),
            Ok(file) => {
                drop(file);
                assert!(Instant::now() < deadline, "holder never took the lock");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(
        error.contains(&format!("Held by pid {}", holder.id())),
        "{error}"
    );
    assert!(error.contains("sleep 30"), "{error}");
}
