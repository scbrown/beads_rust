//! Guarded selected-record effects on a private successor. No fuzzy matching,
//! global orphan cleanup, JSONL publication, or command replay is performed.
use super::{
    CapacityActingContext, CapacityBatchTransition, PendingSyncMergeInspection, SqliteStorage,
};
use crate::{
    BeadsError, Result,
    model::{Dependency, DependencyType, Issue, Status},
    sync,
    validation::IssueValidator,
};
use std::collections::BTreeMap;

fn conflict(message: &str) -> BeadsError {
    BeadsError::SyncConflict {
        message: message.into(),
    }
}

fn canonical(rows: &[Issue]) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut result = BTreeMap::new();
    for row in rows {
        let mut normalized = row.clone();
        sync::normalize_issue_for_export(&mut normalized);
        let value = serde_json::to_value(&normalized)?;
        if result.insert(row.id.clone(), value).is_some()
            || row.ephemeral
            || row.id.contains("-wisp-")
        {
            return Err(conflict("effects require unique ledger identities"));
        }
    }
    Ok(result)
}

fn validate_desired(desired: &[Issue], actor: &str) -> Result<()> {
    if desired.is_empty() || actor.trim().is_empty() || actor.trim() != actor {
        return Err(conflict(
            "nonempty effect selection and explicit actor required",
        ));
    }
    for issue in desired {
        IssueValidator::validate_imported(issue)
            .map_err(|errors| conflict(&format!("invalid effect record: {errors:?}")))?;
        if issue.comments.iter().any(|comment| comment.id <= 0) {
            return Err(conflict("effect comments require reserved positive IDs"));
        }
    }
    Ok(())
}

impl SqliteStorage {
    fn validate_new_dependency_effect(&self, row: &Issue, dep: &Dependency) -> Result<()> {
        if row.id == dep.depends_on_id {
            return Err(BeadsError::SelfDependency { id: row.id.clone() });
        }
        if row.status == Status::Tombstone {
            return Err(conflict("new effect dependency source is a tombstone"));
        }
        Self::ensure_dependency_target_exists_in_tx(&self.conn, &dep.depends_on_id)?;
        Self::validate_parent_child_endpoints(&row.id, &dep.depends_on_id, dep.dep_type.as_str())?;
        if dep.dep_type == DependencyType::ParentChild
            && row
                .dependencies
                .iter()
                .filter(|edge| edge.dep_type == DependencyType::ParentChild)
                .count()
                > 1
        {
            return Err(conflict("effect would give an issue multiple parents"));
        }
        Ok(())
    }

    /// Atomically replace a declared selection only when exact native preimages
    /// match. The caller owns publication and retains the completed source log.
    pub(crate) fn apply_record_effects(
        &mut self,
        before: &[Issue],
        desired: &[Issue],
        actor: &str,
    ) -> Result<()> {
        validate_desired(desired, actor)?;
        let expected = canonical(before)?;
        let after = canonical(desired)?;
        if expected.keys().any(|id| !after.contains_key(id)) {
            return Err(conflict("record effects cannot delete identities"));
        }
        let ids: Vec<_> = after.keys().cloned().collect();
        let policy = self.workflow_capacity_policy.clone();
        let attribution = self.pending_event_attribution_for_review();
        let acting = CapacityActingContext::new(actor, &attribution);
        let timestamp = chrono::Utc::now().to_rfc3339();
        let warnings = self.with_write_transaction(|storage| {
            if !matches!(
                storage.inspect_pending_sync_merge_in_current_transaction()?,
                PendingSyncMergeInspection::Absent
            ) {
                return Err(conflict("pending sync merge prevents record effects"));
            }
            let current = storage.get_issues_for_export(&ids)?;
            let observed = canonical(&current)?;
            if observed != expected {
                return Err(conflict("effect preimage differs; nothing applied"));
            }
            let by_id: BTreeMap<_, _> = current.iter().map(|row| (&row.id, row)).collect();
            let transitions: Vec<_> = desired
                .iter()
                .filter_map(|row| {
                    let old = by_id.get(&row.id);
                    // Type and assignee changes can change scoped/weighted occupancy
                    // even when the status string stays the same.
                    if old.is_some_and(|old| {
                        old.status == row.status
                            && old.issue_type == row.issue_type
                            && old.assignee == row.assignee
                    }) {
                        return None;
                    }
                    Some(CapacityBatchTransition {
                        issue_id: row.id.clone(),
                        from: old.map(|row| row.status.as_str().to_string()),
                        to: row.status.as_str().to_string(),
                        issue_type: Some(row.issue_type.as_str().to_string()),
                        current_assignee: old.and_then(|row| row.assignee.clone()),
                        prospective_assignee: row.assignee.clone(),
                    })
                })
                .collect();
            let warnings = Self::evaluate_workflow_capacity_batch_in_tx(
                &storage.conn,
                &policy,
                &transitions,
                &acting,
            )?;
            let mut new_dependencies = Vec::new();
            storage.delete_comments_for_import_issue_ids_in_tx(&ids)?;
            for row in desired {
                storage.upsert_issue_for_import_in_tx(row)?;
            }
            for row in desired {
                for dep in &row.dependencies {
                    let was_present = by_id.get(&row.id).is_some_and(|old| {
                        old.dependencies.iter().any(|old| {
                            old.depends_on_id == dep.depends_on_id && old.dep_type == dep.dep_type
                        })
                    });
                    if !was_present {
                        storage.validate_new_dependency_effect(row, dep)?;
                        new_dependencies.push((&row.id, dep));
                    }
                }
                storage.sync_labels_for_import_in_tx(&row.id, &row.labels)?;
                storage.sync_dependencies_for_import_in_tx(&row.id, &row.dependencies)?;
                storage.sync_comments_for_import_in_tx(&row.id, &row.comments)?;
            }
            // An SCC report returns one witness, which can stay identical when
            // another cycle is added inside that component. Check each new edge
            // against the final transaction graph using native add-time semantics.
            // Unchanged legacy cycles remain losslessly preserved.
            for (id, dep) in new_dependencies {
                if Self::check_dependency_cycle_for_type(
                    &storage.conn,
                    id,
                    &dep.depends_on_id,
                    &dep.dep_type,
                    true,
                )? {
                    return Err(conflict("record effects introduce a dependency cycle"));
                }
            }
            for id in &ids {
                storage.replace_dirty_issue_marker_in_tx(id, &timestamp)?;
            }
            storage.clear_export_hashes_in_tx(&ids)?;
            storage.set_metadata_in_tx("needs_flush", "true")?;
            storage.rebuild_blocked_cache_in_tx()?;
            storage.rebuild_child_counters_in_tx()?;
            if canonical(&storage.get_issues_for_export(&ids)?)? != after {
                return Err(conflict(
                    "native effect read-back differs; transaction rolled back",
                ));
            }
            Ok(warnings)
        })?;
        self.last_capacity_warnings = warnings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> SqliteStorage {
        let mut storage = SqliteStorage::open_memory().unwrap();
        for id in ["probe-a", "probe-b", "probe-c"] {
            storage
                .create_issue(
                    &Issue {
                        id: id.into(),
                        title: id.into(),
                        ..Issue::default()
                    },
                    "fixture",
                )
                .unwrap();
        }
        storage
    }

    fn edge(source: &str, target: &str, kind: &str) -> Dependency {
        serde_json::from_value(serde_json::json!({
            "issue_id": source, "depends_on_id": target, "type": kind,
            "created_at": "2026-10-03T00:00:00Z",
            "created_by": "fixture", "metadata": "{}", "thread_id": ""
        }))
        .unwrap()
    }

    fn rows(storage: &SqliteStorage, ids: &[&str]) -> Vec<Issue> {
        sync::export_selected_records(
            storage,
            &ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn new_cycle_inside_legacy_component_refuses_without_losing_legacy_edges() {
        let mut storage = fixture();
        storage.execute_test_sql(
            "INSERT INTO dependencies(issue_id,depends_on_id,type,created_at,created_by,metadata,thread_id) VALUES
            ('probe-a','probe-b','blocks','2026-10-03T00:00:00Z','fixture','{}',''),
            ('probe-b','probe-a','blocks','2026-10-03T00:00:00Z','fixture','{}',''),
            ('probe-b','probe-c','blocks','2026-10-03T00:00:00Z','fixture','{}',''),
            ('probe-c','probe-b','blocks','2026-10-03T00:00:00Z','fixture','{}','');",
        ).unwrap();
        let before = rows(&storage, &["probe-a"]);
        let mut related = before.clone();
        related[0].title = "preserve legacy component".into();
        related[0]
            .dependencies
            .push(edge("probe-a", "probe-c", "related"));
        storage
            .apply_record_effects(&before, &related, "effect")
            .unwrap();
        let before = rows(&storage, &["probe-a"]);
        let witness = storage.detect_all_cycles().unwrap();
        assert_eq!(witness.len(), 1, "legacy-cycle positive control");
        let mut after = before.clone();
        after[0]
            .dependencies
            .iter_mut()
            .find(|dep| dep.depends_on_id == "probe-c")
            .unwrap()
            .dep_type = DependencyType::Blocks;
        let error = storage
            .apply_record_effects(&before, &after, "effect")
            .unwrap_err();
        assert!(error.to_string().contains("cycle"), "{error}");
        assert_eq!(
            canonical(&rows(&storage, &["probe-a"])).unwrap(),
            canonical(&before).unwrap()
        );
        assert_eq!(storage.detect_all_cycles().unwrap(), witness);
    }

    #[test]
    fn selected_batch_cycle_is_checked_against_all_final_edges() {
        let mut storage = fixture();
        let before = rows(&storage, &["probe-a", "probe-b"]);
        let mut after = before.clone();
        after[0]
            .dependencies
            .push(edge("probe-a", "probe-b", "blocks"));
        after[1]
            .dependencies
            .push(edge("probe-b", "probe-a", "blocks"));
        assert!(
            storage
                .apply_record_effects(&before, &after, "effect")
                .is_err()
        );
        assert_eq!(
            canonical(&rows(&storage, &["probe-a", "probe-b"])).unwrap(),
            canonical(&before).unwrap()
        );
    }

    #[test]
    fn effect_cannot_add_multiple_parents() {
        let mut storage = fixture();
        let before = rows(&storage, &["probe-a"]);
        let mut after = before.clone();
        after[0].dependencies = vec![
            edge("probe-a", "probe-b", "parent-child"),
            edge("probe-a", "probe-c", "parent-child"),
        ];
        assert!(
            storage
                .apply_record_effects(&before, &after, "effect")
                .is_err()
        );
        assert_eq!(
            canonical(&rows(&storage, &["probe-a"])).unwrap(),
            canonical(&before).unwrap()
        );
    }
}
