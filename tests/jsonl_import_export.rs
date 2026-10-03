mod common;

use beads_rust::model::{Comment, DependencyType, Issue, Priority, Status};
use beads_rust::storage::SqliteStorage;
use beads_rust::sync::{
    ExportConfig, ImportConfig, export_to_jsonl, finalize_export, import_from_jsonl,
    read_issues_from_jsonl,
};
use chrono::{Duration, TimeZone, Utc};
use common::fixtures;
use std::fs;
use tempfile::TempDir;

fn issue_with_id(id: &str, title: &str) -> Issue {
    let mut issue = fixtures::issue(title);
    issue.id = id.to_string();
    issue
}

#[test]
#[allow(clippy::too_many_lines)] // One persisted lifecycle: import, reopen, edit, export, update, remove.
fn unknown_jsonl_fields_survive_reopen_native_edit_and_real_export() {
    use beads_rust::storage::IssueUpdate;
    use serde_json::json;

    let temp = TempDir::new().unwrap();
    let input = temp.path().join("input.jsonl");
    let output = temp.path().join("output.jsonl");
    let db = temp.path().join("store.db");
    let mut raw = serde_json::to_value(issue_with_id("test-carry", "Peer item")).unwrap();
    raw["_seeds"] = json!({"revision": 7, "facts": [{"predicate": "custom", "value": "雪"}]});
    raw["outcome"] = json!(null);
    raw["workflow_run"] = json!({"id": "run-1", "steps": [true, 5, null]});
    fs::write(&input, format!("{raw}\n")).unwrap();
    {
        let mut storage = SqliteStorage::open(&db).unwrap();
        let imported = import_from_jsonl(
            &mut storage,
            &input,
            &ImportConfig::default(),
            Some("test-"),
        )
        .unwrap();
        assert_eq!(imported.imported_count, 1);
    }
    let mut storage = SqliteStorage::open(&db).unwrap();
    let loaded = storage.get_issue("test-carry").unwrap().unwrap();
    assert_eq!(loaded.jsonl_extensions.len(), 3);
    assert_eq!(loaded.jsonl_extensions["_seeds"], raw["_seeds"]);
    storage
        .update_issue(
            "test-carry",
            &IssueUpdate {
                title: Some("Native positive control".into()),
                ..IssueUpdate::default()
            },
            "native-editor",
        )
        .unwrap();
    let result = export_to_jsonl(&storage, &output, &ExportConfig::default()).unwrap();
    assert_eq!(
        result.exported_count, 1,
        "a skipped flush is not a round trip"
    );
    let returned: serde_json::Value =
        serde_json::from_str(fs::read_to_string(&output).unwrap().trim()).unwrap();
    assert_eq!(returned["title"], "Native positive control");
    for name in ["_seeds", "outcome", "workflow_run"] {
        assert!(
            returned.as_object().unwrap().contains_key(name),
            "lost {name}"
        );
        assert_eq!(returned[name], raw[name], "changed {name}");
    }

    // An extension-only newer import must update, while its repeat is a no-op.
    let mut incoming = storage.get_issue_for_export("test-carry").unwrap().unwrap();
    incoming.updated_at += Duration::seconds(1);
    incoming
        .jsonl_extensions
        .insert("_seeds".into(), json!({"revision": 8}));
    fs::write(
        &input,
        format!("{}\n", serde_json::to_string(&incoming).unwrap()),
    )
    .unwrap();
    let updated = import_from_jsonl(
        &mut storage,
        &input,
        &ImportConfig::default(),
        Some("test-"),
    )
    .unwrap();
    assert_eq!(updated.updated_count, 1);
    let repeated = import_from_jsonl(
        &mut storage,
        &input,
        &ImportConfig::default(),
        Some("test-"),
    )
    .unwrap();
    assert_eq!(repeated.updated_count, 0);
    assert_eq!(
        storage
            .get_issue("test-carry")
            .unwrap()
            .unwrap()
            .jsonl_extensions,
        incoming.jsonl_extensions
    );

    // Explicit removal in a newer full snapshot must not resurrect stale carry.
    incoming.updated_at += Duration::seconds(1);
    incoming.jsonl_extensions.clear();
    fs::write(
        &input,
        format!("{}\n", serde_json::to_string(&incoming).unwrap()),
    )
    .unwrap();
    import_from_jsonl(
        &mut storage,
        &input,
        &ImportConfig::default(),
        Some("test-"),
    )
    .unwrap();
    assert!(
        storage
            .get_issue("test-carry")
            .unwrap()
            .unwrap()
            .jsonl_extensions
            .is_empty()
    );
}

#[test]
fn extension_only_changes_participate_in_sync_equality() {
    let base = issue_with_id("test-carry", "Same modeled content");
    let mut peer = base.clone();
    peer.jsonl_extensions
        .insert("future_field".into(), serde_json::json!([null, "value"]));
    assert_eq!(base.compute_content_hash(), peer.compute_content_hash());
    assert!(!base.sync_equals(&peer));
    assert!(peer.sync_equals(&peer.clone()));
}

#[test]
#[allow(clippy::too_many_lines)] // One nested import/edit/export/update/removal lifecycle.
fn nested_extensions_survive_disk_reopen_edit_and_export() {
    use beads_rust::storage::IssueUpdate;
    use serde_json::json;

    let temp = TempDir::new().unwrap();
    let input = temp.path().join("nested.jsonl");
    let output = temp.path().join("export.jsonl");
    let db = temp.path().join("store.db");
    let mut raw = serde_json::to_value(issue_with_id("test-child", "Child")).unwrap();
    raw["comments"] = json!([{"id":7,"issue_id":"test-child","author":"peer",
        "text":"comment control","created_at":"2026-01-01T00:00:00Z",
        "_seeds":{"index":7,"facts":[null,"雪"]},"future":null}]);
    raw["dependencies"] = json!([{"issue_id":"test-child","depends_on_id":"test-parent",
        "type":"blocks","created_at":"2026-01-01T00:00:00Z","created_by":"peer",
        "future":{"nested":[true,null,3]}}]);
    let parent = serde_json::to_string(&issue_with_id("test-parent", "Parent")).unwrap();
    fs::write(&input, format!("{parent}\n{raw}\n")).unwrap();
    {
        let mut storage = SqliteStorage::open(&db).unwrap();
        assert_eq!(
            import_from_jsonl(
                &mut storage,
                &input,
                &ImportConfig::default(),
                Some("test-")
            )
            .unwrap()
            .imported_count,
            2
        );
    }
    let mut storage = SqliteStorage::open(&db).unwrap();
    storage
        .update_issue(
            "test-child",
            &IssueUpdate {
                title: Some("Native edit".into()),
                ..IssueUpdate::default()
            },
            "editor",
        )
        .unwrap();
    storage
        .add_comment("test-child", "editor", "Native comment")
        .unwrap();
    assert!(
        !storage
            .add_dependency("test-child", "test-parent", "blocks", "editor")
            .unwrap()
    );
    assert_eq!(
        export_to_jsonl(&storage, &output, &ExportConfig::default())
            .unwrap()
            .exported_count,
        2
    );
    let returned = read_issues_from_jsonl(&output)
        .unwrap()
        .into_iter()
        .find(|issue| issue.id == "test-child")
        .unwrap();
    assert_eq!(returned.title, "Native edit");
    assert_eq!(
        returned.comments[0].jsonl_extensions["_seeds"],
        raw["comments"][0]["_seeds"]
    );
    assert!(returned.comments[0].jsonl_extensions.contains_key("future"));
    assert_eq!(
        returned.dependencies[0].jsonl_extensions["future"],
        raw["dependencies"][0]["future"]
    );
    let mut changed = returned.clone();
    changed.comments[0].jsonl_extensions.clear();
    assert!(!returned.sync_equals(&changed));
    changed = returned.clone();
    changed.dependencies[0].jsonl_extensions.clear();
    assert!(!returned.sync_equals(&changed));

    let mut incoming = returned.clone();
    incoming.updated_at += Duration::seconds(1);
    incoming.comments[0]
        .jsonl_extensions
        .insert("_seeds".into(), json!({"index":8}));
    incoming.dependencies[0]
        .jsonl_extensions
        .insert("future".into(), json!(["updated"]));
    for remove in [false, true] {
        if remove {
            incoming.updated_at += Duration::seconds(1);
            incoming.comments[0].jsonl_extensions.clear();
            incoming.dependencies[0].jsonl_extensions.clear();
        }
        fs::write(
            &input,
            format!("{parent}\n{}\n", serde_json::to_string(&incoming).unwrap()),
        )
        .unwrap();
        assert_eq!(
            import_from_jsonl(
                &mut storage,
                &input,
                &ImportConfig::default(),
                Some("test-")
            )
            .unwrap()
            .updated_count,
            1
        );
        assert_eq!(
            import_from_jsonl(
                &mut storage,
                &input,
                &ImportConfig::default(),
                Some("test-")
            )
            .unwrap()
            .updated_count,
            0
        );
        let actual = storage.get_issue_for_export("test-child").unwrap().unwrap();
        assert_eq!(actual.comments, incoming.comments);
        assert_eq!(actual.dependencies, incoming.dependencies);
    }
}

#[test]
fn export_import_roundtrip_preserves_relationships() {
    let mut storage = SqliteStorage::open_memory().unwrap();
    let mut alpha = fixtures::issue("Alpha");
    // Ensure created_at is strictly before any updates (SQLite CURRENT_TIMESTAMP has low precision)
    alpha.created_at = Utc::now() - Duration::hours(1);
    alpha.updated_at = alpha.created_at;
    let mut beta = fixtures::issue("Beta");
    beta.created_at = alpha.created_at;
    beta.updated_at = alpha.created_at;

    alpha.priority = Priority::HIGH;
    alpha.external_ref = Some("ext-1".to_string());
    beta.status = Status::InProgress;

    storage.create_issue(&alpha, "tester").unwrap();
    storage.create_issue(&beta, "tester").unwrap();
    storage
        .add_dependency(
            &beta.id,
            &alpha.id,
            DependencyType::Blocks.as_str(),
            "tester",
        )
        .unwrap();
    storage.add_label(&alpha.id, "alpha", "tester").unwrap();
    storage
        .add_comment(&alpha.id, "tester", "first comment")
        .unwrap();

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    let export = export_to_jsonl(&storage, &path, &ExportConfig::default()).unwrap();
    assert_eq!(export.exported_count, 2);

    let mut imported = SqliteStorage::open_memory().unwrap();
    let import = import_from_jsonl(
        &mut imported,
        &path,
        &ImportConfig::default(),
        Some("test-"),
    )
    .unwrap();
    assert_eq!(import.imported_count, 2);

    let imported_alpha = imported.get_issue(&alpha.id).unwrap().unwrap();
    assert_eq!(imported_alpha.title, alpha.title);
    assert_eq!(imported_alpha.external_ref, Some("ext-1".to_string()));

    let labels = imported.get_labels(&alpha.id).unwrap();
    assert_eq!(labels, vec!["alpha".to_string()]);

    let deps = imported.get_dependencies(&beta.id).unwrap();
    assert_eq!(deps, vec![alpha.id.clone()]);

    let comments = imported.get_comments(&alpha.id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].body, "first comment");
}

#[test]
fn import_reads_multiple_jsonl_lines_without_buffer_accumulation() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    let issue_a = issue_with_id("test-a", "First");
    let issue_b = issue_with_id("test-b", "Second");
    let json_a = serde_json::to_string(&issue_a).unwrap();
    let json_b = serde_json::to_string(&issue_b).unwrap();
    fs::write(&path, format!("{json_a}\n{json_b}\n")).unwrap();

    let mut storage = SqliteStorage::open_memory().unwrap();
    let import =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();

    assert_eq!(import.imported_count, 2);
    assert!(storage.get_issue("test-a").unwrap().is_some());
    assert!(storage.get_issue("test-b").unwrap().is_some());
}

#[test]
fn export_sorts_by_id() {
    let mut storage = SqliteStorage::open_memory().unwrap();
    let issue_b = issue_with_id("test-b", "B");
    let issue_a = issue_with_id("test-a", "A");

    storage.create_issue(&issue_b, "tester").unwrap();
    storage.create_issue(&issue_a, "tester").unwrap();

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    export_to_jsonl(&storage, &path, &ExportConfig::default()).unwrap();

    let issues = read_issues_from_jsonl(&path).unwrap();
    let ids: Vec<&str> = issues.iter().map(|issue| issue.id.as_str()).collect();
    assert_eq!(ids, vec!["test-a", "test-b"]);
}

#[test]
fn export_sorts_comments_canonically_after_loading_relations() {
    let mut storage = SqliteStorage::open_memory().unwrap();
    let timestamp = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
    let mut issue = issue_with_id("test-comment-sort", "Comment sort");
    issue.created_at = timestamp;
    issue.updated_at = timestamp;
    issue.comments = vec![
        Comment {
            jsonl_extensions: std::collections::BTreeMap::new(),
            id: 0,
            issue_id: issue.id.clone(),
            author: "zara".to_string(),
            body: "second by canonical order".to_string(),
            created_at: timestamp,
        },
        Comment {
            jsonl_extensions: std::collections::BTreeMap::new(),
            id: 0,
            issue_id: issue.id.clone(),
            author: "alice".to_string(),
            body: "first by canonical order".to_string(),
            created_at: timestamp,
        },
    ];

    storage.create_issue(&issue, "tester").unwrap();

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    export_to_jsonl(&storage, &path, &ExportConfig::default()).unwrap();

    let issues = read_issues_from_jsonl(&path).unwrap();
    assert_eq!(issues.len(), 1);
    let authors: Vec<&str> = issues[0]
        .comments
        .iter()
        .map(|comment| comment.author.as_str())
        .collect();
    assert_eq!(authors, vec!["alice", "zara"]);
}

#[test]
fn import_rejects_malformed_json() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    fs::write(&path, "not json\n").unwrap();

    let mut storage = SqliteStorage::open_memory().unwrap();
    let err = import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-"))
        .unwrap_err();
    assert!(err.to_string().contains("Invalid JSON"));
}

#[test]
fn import_rejects_prefix_mismatch() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    let issue = issue_with_id("xx-001", "Mismatch");
    let json = serde_json::to_string(&issue).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    let mut storage = SqliteStorage::open_memory().unwrap();
    let err = import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-"))
        .unwrap_err();
    assert!(err.to_string().contains("Prefix mismatch"));
}

#[test]
fn import_sets_closed_at_when_missing() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    let mut issue = issue_with_id("test-closed", "Closed");
    issue.status = Status::Closed;
    issue.created_at = Utc::now() - Duration::hours(2);
    issue.updated_at = Utc::now() - Duration::hours(1);
    issue.closed_at = None;
    let json = serde_json::to_string(&issue).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    let mut storage = SqliteStorage::open_memory().unwrap();
    import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();

    let imported = storage.get_issue(&issue.id).unwrap().unwrap();
    assert_eq!(imported.closed_at, Some(issue.updated_at));
}

#[test]
fn export_import_roundtrip_keeps_optional_text_fields_integrity_safe() {
    let mut storage = SqliteStorage::open_memory().unwrap();
    let issue = issue_with_id("test-opttext", "Optional text fields");
    storage.create_issue(&issue, "tester").unwrap();

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    export_to_jsonl(&storage, &path, &ExportConfig::default()).unwrap();

    let db_path = temp.path().join("import.db");
    let mut imported = SqliteStorage::open(&db_path).unwrap();
    import_from_jsonl(
        &mut imported,
        &path,
        &ImportConfig::default(),
        Some("test-"),
    )
    .unwrap();

    let imported_issue = imported.get_issue(&issue.id).unwrap().unwrap();
    assert_eq!(imported_issue.design, None);
    assert_eq!(imported_issue.acceptance_criteria, None);
}

#[test]
fn import_rejects_conflict_markers() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    fs::write(&path, "<<<<<<< HEAD\n").unwrap();

    let mut storage = SqliteStorage::open_memory().unwrap();
    let err = import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-"))
        .unwrap_err();
    assert!(err.to_string().contains("Merge conflict markers detected"));
}

// ===== Safety Guard Tests =====

#[test]
fn export_empty_db_guard_blocks_overwrite() {
    // Empty database should not overwrite non-empty JSONL without --force
    let storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create existing JSONL with content
    let existing = issue_with_id("test-existing", "Existing issue");
    let json = serde_json::to_string(&existing).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Try to export empty database (should fail)
    let config = ExportConfig {
        force: false,
        ..Default::default()
    };
    let result = export_to_jsonl(&storage, &path, &config);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("empty database"),
        "Expected 'empty database' error, got: {err}"
    );
}

#[test]
fn export_empty_db_guard_bypassed_with_force() {
    // Empty database CAN overwrite non-empty JSONL with --force
    let storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create existing JSONL with content
    let existing = issue_with_id("test-existing", "Existing issue");
    let json = serde_json::to_string(&existing).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Export empty database with force (should succeed)
    let config = ExportConfig {
        force: true,
        ..Default::default()
    };
    let result = export_to_jsonl(&storage, &path, &config);

    assert!(result.is_ok());
    let export = result.unwrap();
    assert_eq!(export.exported_count, 0);
}

// ===== Tombstone Protection Tests =====

#[test]
fn import_tombstone_protection_prevents_resurrection() {
    // Tombstones in DB should never be resurrected by import
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create a tombstone in the database
    let mut tombstone = issue_with_id("test-tomb", "Tombstone issue");
    tombstone.status = Status::Tombstone;
    tombstone.deleted_at = Some(Utc::now());
    storage.create_issue(&tombstone, "tester").unwrap();

    // Create JSONL trying to resurrect the tombstone
    let mut incoming = issue_with_id("test-tomb", "Resurrected issue");
    incoming.status = Status::Open;
    incoming.updated_at = Utc::now() + Duration::hours(1);
    let json = serde_json::to_string(&incoming).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import should skip the tombstone
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.tombstone_skipped, 1);

    // Verify the issue is still a tombstone
    let still_tombstone = storage.get_issue("test-tomb").unwrap().unwrap();
    assert_eq!(still_tombstone.status, Status::Tombstone);
}

// ===== Collision Detection Tests =====

#[test]
fn import_collision_by_id_updates_when_newer() {
    // When importing an issue with same ID but newer timestamp, it should update
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create existing issue with older timestamp
    let mut existing = issue_with_id("test-001", "Old title");
    existing.updated_at = Utc::now() - Duration::hours(1);
    storage.create_issue(&existing, "tester").unwrap();

    // Create JSONL with same ID but newer timestamp
    let mut incoming = issue_with_id("test-001", "New title");
    incoming.updated_at = Utc::now();
    let json = serde_json::to_string(&incoming).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import should update
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.imported_count, 1);

    // Verify the update
    let updated = storage.get_issue("test-001").unwrap().unwrap();
    assert_eq!(updated.title, "New title");
}

#[test]
fn import_collision_by_id_skips_when_older() {
    // When importing an issue with same ID but older timestamp, it should skip
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create existing issue with newer timestamp
    let mut existing = issue_with_id("test-001", "Newer title");
    existing.created_at = Utc::now() - Duration::hours(2);
    existing.updated_at = Utc::now();
    storage.create_issue(&existing, "tester").unwrap();

    // Create JSONL with same ID but older timestamp
    let mut incoming = issue_with_id("test-001", "Older title");
    incoming.created_at = existing.created_at;
    incoming.updated_at = Utc::now() - Duration::hours(1);
    let json = serde_json::to_string(&incoming).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import should skip
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.skipped_count, 1);

    // Verify no change
    let unchanged = storage.get_issue("test-001").unwrap().unwrap();
    assert_eq!(unchanged.title, "Newer title");
}

#[test]
fn import_collision_by_external_ref() {
    // When importing an issue with matching external_ref, it should match (phase 1)
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create existing issue with external_ref
    let mut existing = issue_with_id("test-001", "Existing");
    existing.external_ref = Some("JIRA-123".to_string());
    existing.updated_at = Utc::now() - Duration::hours(1);
    storage.create_issue(&existing, "tester").unwrap();

    // Create JSONL with SAME external_ref and same ID, newer timestamp
    let mut incoming = issue_with_id("test-001", "Incoming updated");
    incoming.external_ref = Some("JIRA-123".to_string());
    incoming.updated_at = Utc::now();
    let json = serde_json::to_string(&incoming).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import should update (matched by external_ref in phase 1)
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.imported_count, 1);

    // Verify the update
    let updated = storage.get_issue("test-001").unwrap().unwrap();
    assert_eq!(updated.title, "Incoming updated");
}

// ===== Ephemeral Issue Tests =====

#[test]
fn import_skips_ephemeral_issues() {
    // Ephemeral issues should be skipped during import
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create JSONL with ephemeral issue
    let mut ephemeral = issue_with_id("test-eph", "Ephemeral issue");
    ephemeral.ephemeral = true;
    let json = serde_json::to_string(&ephemeral).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import should skip
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.skipped_count, 1);
    assert_eq!(result.imported_count, 0);

    // Verify the issue was not created
    assert!(storage.get_issue("test-eph").unwrap().is_none());
}

// ===== Prefix Validation Tests =====

#[test]
fn import_skip_prefix_validation_allows_mismatch() {
    // With skip_prefix_validation, mismatched prefixes should be allowed
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create JSONL with different prefix
    let issue = issue_with_id("other-001", "Different prefix");
    let json = serde_json::to_string(&issue).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import with skip_prefix_validation should succeed
    let config = ImportConfig {
        skip_prefix_validation: true,
        ..Default::default()
    };
    let result = import_from_jsonl(&mut storage, &path, &config, Some("test-")).unwrap();
    assert_eq!(result.imported_count, 1);
}

// ===== Deterministic Export Tests =====

#[test]
fn export_produces_deterministic_content_hash() {
    // Multiple exports of the same data should produce the same content hash
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();

    // Create test issue
    let issue = issue_with_id("test-det", "Deterministic test");
    storage.create_issue(&issue, "tester").unwrap();

    let config = ExportConfig::default();

    // Export twice to different files
    let path1 = temp.path().join("export1.jsonl");
    let path2 = temp.path().join("export2.jsonl");

    let result1 = export_to_jsonl(&storage, &path1, &config).unwrap();
    let result2 = export_to_jsonl(&storage, &path2, &config).unwrap();

    // Hashes should be identical
    assert_eq!(result1.content_hash, result2.content_hash);
    assert!(!result1.content_hash.is_empty());
}

// ===== Empty Lines Handling Tests =====

#[test]
fn import_handles_empty_lines_gracefully() {
    // JSONL with empty lines interspersed should still import correctly
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create JSONL with empty lines
    let issue = issue_with_id("test-001", "Valid issue");
    let json = serde_json::to_string(&issue).unwrap();
    let content = format!("\n\n{json}\n\n\n");
    fs::write(&path, content).unwrap();

    // Import should succeed, ignoring empty lines
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.imported_count, 1);
}

// ===== New Issue Creation Tests =====

#[test]
fn import_creates_new_issues() {
    // New issues (not in DB) should be created
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create JSONL with new issue
    let issue = issue_with_id("test-new", "Brand new issue");
    let json = serde_json::to_string(&issue).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    // Import should create the issue
    let result =
        import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();
    assert_eq!(result.imported_count, 1);
    assert_eq!(result.skipped_count, 0);

    // Verify the issue exists
    let created = storage.get_issue("test-new").unwrap().unwrap();
    assert_eq!(created.title, "Brand new issue");
}

#[test]
fn import_repopulates_export_hashes() {
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    // Create and export an issue
    let issue = issue_with_id("test-hash", "Hash Test");
    storage.create_issue(&issue, "tester").unwrap();
    let export_result = export_to_jsonl(&storage, &path, &ExportConfig::default()).unwrap();
    finalize_export(
        &mut storage,
        &export_result,
        Some(&export_result.issue_hashes),
        &path,
    )
    .unwrap();
    let original_hash = export_result.issue_hashes[0].1.clone();

    // Verify hash exists
    assert_eq!(
        storage.get_export_hash("test-hash").unwrap().unwrap().0,
        original_hash
    );

    // Clear hash manually
    storage.clear_all_export_hashes().unwrap();
    assert!(storage.get_export_hash("test-hash").unwrap().is_none());

    // Import the file back
    import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();

    // Verify hash is restored
    let (restored_hash, _) = storage.get_export_hash("test-hash").unwrap().unwrap();
    assert_eq!(restored_hash, original_hash);
}

#[test]
fn import_deduplicates_export_hash_rebuild_when_multiple_records_target_same_issue() {
    let mut storage = SqliteStorage::open_memory().unwrap();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");

    let base_time = Utc::now() - Duration::hours(2);
    let mut existing = issue_with_id("test-existing", "Existing");
    existing.created_at = base_time;
    existing.updated_at = base_time;
    existing.external_ref = Some("EXT-1".to_string());
    storage.create_issue(&existing, "tester").unwrap();
    storage
        .set_export_hashes(&[("test-existing".to_string(), "stale-hash".to_string())])
        .unwrap();

    let mut by_external_ref = issue_with_id("test-remap", "Intermediate update");
    by_external_ref.created_at = base_time + Duration::minutes(5);
    by_external_ref.updated_at = base_time + Duration::minutes(10);
    by_external_ref.external_ref = Some("EXT-1".to_string());

    let mut by_id = issue_with_id("test-existing", "Final update");
    by_id.created_at = base_time + Duration::minutes(15);
    by_id.updated_at = base_time + Duration::minutes(20);

    let json = format!(
        "{}\n{}\n",
        serde_json::to_string(&by_external_ref).unwrap(),
        serde_json::to_string(&by_id).unwrap()
    );
    fs::write(&path, json).unwrap();

    import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-")).unwrap();

    assert!(
        storage.get_issue("test-remap").unwrap().is_none(),
        "collision-matched issue should be merged into the existing record"
    );

    let imported = storage.get_issue("test-existing").unwrap().unwrap();
    assert_eq!(imported.title, "Final update");

    let (stored_hash, _) = storage.get_export_hash("test-existing").unwrap().unwrap();
    assert_eq!(Some(stored_hash.as_str()), imported.content_hash.as_deref());
}

#[test]
fn import_rejects_invalid_id_format() {
    // Import now validates issues, so invalid IDs should be rejected.
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("issues.jsonl");
    let issue = issue_with_id("test-INVALID", "Invalid ID");
    let json = serde_json::to_string(&issue).unwrap();
    fs::write(&path, format!("{json}\n")).unwrap();

    let mut storage = SqliteStorage::open_memory().unwrap();
    let result = import_from_jsonl(&mut storage, &path, &ImportConfig::default(), Some("test-"));

    assert!(result.is_err(), "Import should fail for invalid IDs");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Validation failed"),
        "Expected validation error, got: {err}"
    );
}
