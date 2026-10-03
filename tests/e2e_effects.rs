//! Native completion receipts bind exact before/after images to normal writes.
mod common;
use common::cli::{BrWorkspace, run_br};
use serde_json::Value;

#[test]
fn receipted_label_reuses_exact_owner_and_preserves_native_validation() {
    let w = BrWorkspace::new();
    assert!(run_br(&w, ["init"], "init").status.success());
    let created = run_br(&w, ["create", "receipt label", "--silent"], "create");
    assert!(created.status.success(), "{}", created.stderr);
    let id = created.stdout.trim();
    let db = w.root.join(".beads/beads.db");
    let receipt = w.root.join("label-effect.json");
    let before = run_br(&w, ["show", "--lossless", id], "before");
    assert!(before.status.success(), "{}", before.stderr);
    let result = run_br(
        &w,
        [
            "--db",
            db.to_str().unwrap(),
            "--no-auto-import",
            "--no-auto-flush",
            "--effect-receipt",
            receipt.to_str().unwrap(),
            "label",
            "add",
            id,
            "verified-owner",
        ],
        "label-write",
    );
    assert!(result.status.success(), "{}", result.stderr);
    let evidence: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    let after = run_br(
        &w,
        [
            "--no-auto-import",
            "--no-auto-flush",
            "show",
            "--lossless",
            id,
        ],
        "after",
    );
    assert!(after.status.success(), "{}", after.stderr);
    assert_eq!(
        evidence["after"]["records"][id],
        serde_json::from_str::<Value>(&after.stdout).unwrap()
    );
    assert_eq!(
        evidence["after"]["records"][id]["labels"],
        serde_json::json!(["verified-owner"])
    );
    assert_eq!(
        evidence["before"]["records"][id],
        serde_json::from_str::<Value>(&before.stdout).unwrap()
    );
    for (name, input, label) in [
        ("invalid-label", id, "bad label"),
        ("inexact-owner", &id[1..], "other"),
    ] {
        let refused_receipt = w.root.join(format!("{name}.json"));
        let refused = run_br(
            &w,
            [
                "--db",
                db.to_str().unwrap(),
                "--no-auto-import",
                "--no-auto-flush",
                "--effect-receipt",
                refused_receipt.to_str().unwrap(),
                "label",
                "add",
                input,
                label,
            ],
            name,
        );
        assert!(!refused.status.success(), "{name} unexpectedly succeeded");
        assert!(!refused_receipt.exists());
    }
    let unchanged = run_br(
        &w,
        [
            "--no-auto-import",
            "--no-auto-flush",
            "show",
            "--lossless",
            id,
        ],
        "unchanged",
    );
    assert!(unchanged.status.success(), "{}", unchanged.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&after.stdout).unwrap(),
        serde_json::from_str::<Value>(&unchanged.stdout).unwrap()
    );
}

#[test]
fn selected_effects_require_preimage_and_preserve_unselected_records() {
    let w = BrWorkspace::new();
    assert!(run_br(&w, ["init"], "init").status.success());
    let created = run_br(&w, ["create", "selected", "--silent"], "create");
    let id = created.stdout.trim();
    let other = run_br(&w, ["create", "untouched", "--silent"], "other");
    let other_id = other.stdout.trim();
    assert!(
        run_br(&w, ["dep", "add", "--type", "related", id, other_id], "dep")
            .status
            .success()
    );
    assert!(
        run_br(&w, ["comments", "add", id, "preserve comment"], "comment")
            .status
            .success()
    );
    let read = |id: &str, label: &str| {
        let r = run_br(
            &w,
            [
                "--no-auto-import",
                "--no-auto-flush",
                "show",
                "--lossless",
                id,
            ],
            label,
        );
        assert!(r.status.success(), "{}", r.stderr);
        serde_json::from_str::<Value>(&r.stdout).unwrap()
    };
    let before = read(id, "before");
    let untouched = read(other_id, "untouched-before");
    let mut after = before.clone();
    after["title"] = serde_json::json!("verified effect title");
    after["future"] = serde_json::json!({"nullable":null,"nested":[1,true]});
    let image = |row: Value| serde_json::json!({"format":"br-native-effects-v1","ids":[id],"records":{id:row}});
    let input = serde_json::json!({"before":image(before.clone()),"after":image(after.clone())});
    let file = w.root.join("delta.json");
    std::fs::write(&file, input.to_string()).unwrap();
    let db = w.root.join(".beads/beads.db");
    let apply = |label| {
        run_br(
            &w,
            [
                "--db",
                db.to_str().unwrap(),
                "--actor",
                "verified-effect",
                "--no-auto-import",
                "--no-auto-flush",
                "sync",
                "--effects",
                file.to_str().unwrap(),
            ],
            label,
        )
    };
    let applied = apply("apply");
    assert!(
        applied.status.success(),
        "{} {}",
        applied.stdout,
        applied.stderr
    );
    assert_eq!(read(id, "after"), after);
    assert_eq!(read(other_id, "untouched-after"), untouched);
    let stale = apply("stale");
    assert!(!stale.status.success(), "stale preimage accepted");
    assert_eq!(read(id, "after-stale"), after);
}

#[test]
fn completion_receipt_matches_lossless_export_and_refuses_overwrite() {
    let w = BrWorkspace::new();
    assert!(run_br(&w, ["init"], "init").status.success());
    let created = run_br(&w, ["create", "receipt", "--silent"], "create");
    assert!(created.status.success(), "{}", created.stderr);
    let id = created.stdout.trim();
    let db = w.root.join(".beads/beads.db");
    let receipt = w.root.join("effect.json");
    let db = db.to_str().unwrap();
    let path = receipt.to_str().unwrap();
    let before = run_br(&w, ["show", "--lossless", id], "before");
    assert!(before.status.success(), "{}", before.stderr);
    let changed = run_br(
        &w,
        [
            "--db",
            db,
            "--no-auto-import",
            "--no-auto-flush",
            "--effect-receipt",
            path,
            "comments",
            "add",
            id,
            "completed-once",
        ],
        "write",
    );
    assert!(changed.status.success(), "{}", changed.stderr);
    let evidence: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(receipt.with_extension("pending"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let after = run_br(
        &w,
        [
            "--no-auto-import",
            "--no-auto-flush",
            "show",
            "--lossless",
            id,
        ],
        "after",
    );
    assert!(after.status.success(), "{}", after.stderr);
    assert_eq!(
        evidence["before"]["records"][id],
        serde_json::from_str::<Value>(&before.stdout).unwrap()
    );
    assert_eq!(
        evidence["after"]["records"][id],
        serde_json::from_str::<Value>(&after.stdout).unwrap()
    );
    assert_eq!(
        evidence["after"]["records"][id]["comments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let refused = run_br(
        &w,
        [
            "--db",
            db,
            "--no-auto-import",
            "--no-auto-flush",
            "--effect-receipt",
            path,
            "comments",
            "add",
            id,
            "must-not-run",
        ],
        "occupied",
    );
    assert!(!refused.status.success());
    let unchanged = run_br(
        &w,
        [
            "--no-auto-import",
            "--no-auto-flush",
            "show",
            "--lossless",
            id,
        ],
        "unchanged",
    );
    assert_eq!(after.stdout, unchanged.stdout);
}

#[test]
fn failed_native_command_never_emits_completion() {
    let w = BrWorkspace::new();
    assert!(run_br(&w, ["init"], "init").status.success());
    let created = run_br(&w, ["create", "guard", "--silent"], "create");
    assert!(created.status.success());
    let id = created.stdout.trim();
    let db = w.root.join(".beads/beads.db");
    let receipt = w.root.join("failed.json");
    // Claim preconditions stay native: the second actor cannot steal a claim.
    assert!(
        run_br(&w, ["--actor", "first", "update", "--claim", id], "claim")
            .status
            .success()
    );
    let result = run_br(
        &w,
        [
            "--db",
            db.to_str().unwrap(),
            "--actor",
            "second",
            "--no-auto-import",
            "--no-auto-flush",
            "--effect-receipt",
            receipt.to_str().unwrap(),
            "update",
            "--claim",
            id,
        ],
        "refused-claim",
    );
    assert!(!result.status.success());
    assert!(!receipt.exists());
    assert!(receipt.with_extension("pending").exists());
}
