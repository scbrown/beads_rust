//! A search that imported fresh JSONL must release startup write authority.

mod common;

use common::cli::{BrWorkspace, run_br};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct SearchChild(Child);

impl Drop for SearchChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn search_releases_startup_authority_after_auto_import() {
    let workspace = BrWorkspace::new();
    let init = run_br(&workspace, ["init", "--prefix", "probe"], "init");
    assert!(init.status.success(), "{}", init.stderr);

    // Force writable auto-import before search. Output exceeds pipe capacity,
    // so receiving its first byte gives a deterministic read-phase rendezvous.
    let mut jsonl = String::new();
    for n in 0..100 {
        let issue = serde_json::json!({
            "id": format!("probe-{n:04}"),
            "title": "Recovered fixture",
            "description": "x".repeat(4096),
            "status": "open", "priority": 2, "issue_type": "task",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        jsonl.push_str(&issue.to_string());
        jsonl.push('\n');
    }
    std::fs::write(workspace.root.join(".beads/issues.jsonl"), jsonl).unwrap();
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("br"));
    command
        .current_dir(&workspace.root)
        .env_clear()
        .env("HOME", &workspace.root)
        .args(["search", "Recovered", "--all", "--limit", "0", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut search = SearchChild(command.spawn().unwrap());
    let mut stdout = search.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut first = [0];
        let result = stdout.read_exact(&mut first);
        let _ = sender.send((result, stdout));
    });
    let (read, _held_pipe) = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("search must reach output after importing the fixture");
    read.expect("search must return an output byte");
    reader.join().unwrap();
    assert!(
        search.0.try_wait().unwrap().is_none(),
        "search must still be alive"
    );

    let writer = run_br(
        &workspace,
        [
            "--lock-timeout",
            "500",
            "--no-auto-flush",
            "create",
            "Concurrent writer control",
        ],
        "writer_during_search",
    );
    assert!(
        writer.status.success(),
        "search retained startup authority while rendering: {} {}",
        writer.stdout,
        writer.stderr
    );

    // The pipe remains full. This is an actually stuck read-only search, not
    // a pre-cancelled query that merely proves the deadline was configured.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = search.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "search watchdog did not terminate blocked output"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(124));
    let readback = run_br(
        &workspace,
        ["list", "--json", "--limit", "0"],
        "read_after_timeout",
    );
    assert!(readback.status.success(), "{}", readback.stderr);
    assert!(readback.stdout.contains("Concurrent writer control"));
}
