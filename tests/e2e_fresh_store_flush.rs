//! A first write must publish immediately without bypassing another writer.
use beads_rust::sync::blocking_jsonl_family_write_lock_with_timeout;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn fresh_store(root: &Path) -> std::path::PathBuf {
    let beads = root.join(".beads");
    fs::create_dir_all(&beads).unwrap();
    fs::write(
        beads.join("metadata.json"),
        r#"{"database":"beads.db","jsonl_export":"issues.jsonl"}"#,
    )
    .unwrap();
    fs::write(beads.join("config.yaml"), "issue_prefix: probe\n").unwrap();
    fs::write(beads.join("issues.jsonl"), "").unwrap();
    beads
}

fn create(root: &Path, title: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_br"))
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .args([
            "--db",
            root.join(".beads/beads.db").to_str().unwrap(),
            "--lock-timeout",
            "100",
            "--actor",
            "fixture",
            "create",
            title,
            "--json",
        ])
        .output()
        .unwrap()
}

#[test]
fn first_create_publishes_its_jsonl_before_a_second_write() {
    let temp = TempDir::new().unwrap();
    let beads = fresh_store(temp.path());
    assert!(!beads.join("beads.db").exists());
    let first = create(temp.path(), "First fresh write");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&first.stderr).contains("AUTO_FLUSH_FAILED"),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let record: Value = serde_json::from_slice(&first.stdout).unwrap();
    let lines = fs::read_to_string(beads.join("issues.jsonl")).unwrap();
    assert_eq!(lines.lines().count(), 1);
    let exported: Value = serde_json::from_str(&lines).unwrap();
    assert_eq!(exported["id"], record["id"]);
    assert_eq!(exported["title"], "First fresh write");
    let second = create(temp.path(), "Second write");
    assert!(second.status.success());
    assert!(!String::from_utf8_lossy(&second.stderr).contains("AUTO_FLUSH_FAILED"));
    assert_eq!(
        fs::read_to_string(beads.join("issues.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn externally_held_jsonl_authority_blocks_fresh_create_without_publication() {
    let temp = TempDir::new().unwrap();
    let beads = fresh_store(temp.path());
    let jsonl = beads.join("issues.jsonl");
    let _held = blocking_jsonl_family_write_lock_with_timeout(&jsonl, Some(100)).unwrap();
    let blocked = create(temp.path(), "Must not publish");
    assert!(!blocked.status.success());
    let diagnostic: Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(diagnostic["error"]["code"], "CONFIG_ERROR");
    assert!(
        diagnostic["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JSONL-family write lock")
    );
    assert_eq!(fs::read_to_string(&jsonl).unwrap(), "");
    assert!(!beads.join("beads.db").exists());
}
