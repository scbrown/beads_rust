//! Opt-in completed-effect evidence around normal command execution.
//! This is not a command replay API or a multi-store publication protocol.
use super::{Cli, Commands, CommentCommands, DepCommands, LabelCommands};
use crate::{BeadsError, Result, config, storage::SqliteStorage, sync};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// A prepared receipt. The caller holds database-family authority until finish.
pub struct Receipt {
    path: PathBuf,
    database: PathBuf,
    ids: Vec<String>,
    before: Value,
    storage: SqliteStorage,
}

fn refused(message: &str) -> BeadsError {
    BeadsError::Config(message.into())
}

fn image(storage: &SqliteStorage, ids: &[String]) -> Result<Value> {
    let records = sync::export_selected_records(storage, ids)?;
    let rows: serde_json::Map<String, Value> = records
        .into_iter()
        .map(|row| Ok((row.id.clone(), serde_json::to_value(row)?)))
        .collect::<Result<_>>()?;
    Ok(json!({"format":"br-native-effects-v1", "ids":ids, "records":rows}))
}

impl Receipt {
    /// Validate exact local scope and persist a preimage before any command write.
    ///
    /// # Errors
    /// Refuses unsupported commands, routing, fuzzy IDs, and occupied output paths.
    pub fn prepare(cli: &Cli, db: &Path, beads_dir: &Path) -> Result<Self> {
        if cli.db.is_none() || cli.no_db || !cli.no_auto_import || !cli.no_auto_flush {
            return Err(refused(
                "effect receipts require explicit --db and both --no-auto flags",
            ));
        }
        // The main process already holds database-family write authority. The
        // capture connection needs only indexed reads, not a second writable
        // schema/cache initialization. Refuse rather than migrate/fall back.
        let storage = SqliteStorage::open_current_read_only(db)?
            .ok_or_else(|| refused("effect receipt requires a current readable database"))?;
        let (mut ids, references) = match &cli.command {
            Commands::Update(a) if a.ids.len() == 1 && a.parent.is_none() => {
                (a.ids.clone(), a.ids.clone())
            }
            Commands::Comments(a) => match &a.command {
                Some(CommentCommands::Add(a)) => (vec![a.id.clone()], vec![a.id.clone()]),
                _ => return Err(refused("effect receipts support comment add only")),
            },
            Commands::Label {
                command: LabelCommands::Add(a),
            } => {
                let ids = if a.label.is_some() {
                    a.issues.clone()
                } else {
                    a.issues
                        .get(..a.issues.len().saturating_sub(1))
                        .unwrap_or_default()
                        .to_vec()
                };
                if ids.len() != 1 {
                    return Err(refused("effect receipts require one label target"));
                }
                (ids.clone(), ids)
            }
            Commands::Dep {
                command: DepCommands::Add(a),
            } if matches!(a.dep_type.as_str(), "blocks" | "related") => (
                vec![a.issue.clone()],
                vec![a.issue.clone(), a.depends_on.clone()],
            ),
            _ => return Err(refused("command is outside completed-effect scope")),
        };
        if config::routing::group_issue_inputs_by_route(&references, beads_dir)?
            .iter()
            .any(|batch| batch.is_external)
        {
            return Err(refused("effect receipts require local references"));
        }
        // The before image below validates selected owners. Read only additional
        // dependency targets here, avoiding a duplicate capture on every write.
        for id in references.iter().filter(|id| !ids.contains(id)) {
            sync::export_selected_records(&storage, std::slice::from_ref(id))?;
        }
        ids.sort();
        let path = cli
            .effect_receipt
            .as_ref()
            .ok_or_else(|| refused("receipt path required"))?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let path = parent.canonicalize()?.join(
            path.file_name()
                .ok_or_else(|| refused("receipt file required"))?,
        );
        if path.exists() || path.is_symlink() {
            return Err(refused("receipt output already exists"));
        }
        let before = image(&storage, &ids)?;
        let pending = path.with_extension("pending");
        if pending == path {
            return Err(refused("receipt may not have .pending extension"));
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&pending)?;
        file.write_all(&serde_json::to_vec(
            &json!({"phase":"prepared", "before":before}),
        )?)?;
        file.sync_all()?;
        fs::File::open(
            path.parent()
                .ok_or_else(|| refused("receipt parent missing"))?,
        )?
        .sync_all()?;
        Ok(Self {
            path,
            database: db.canonicalize()?,
            ids,
            before,
            storage,
        })
    }

    /// Reuse an exact owner already read under the caller's uninterrupted write
    /// authority. This proof is command-local, never an ID cache across writes.
    pub(crate) fn selected_id(&self, database: &Path, input: &str) -> Result<String> {
        if database.canonicalize()? != self.database
            || !self.ids.iter().any(|id| id == input)
            || self.before["records"].get(input).is_none()
        {
            return Err(refused(
                "prepared receipt does not witness this database and exact ID",
            ));
        }
        Ok(input.to_owned())
    }

    /// Publish completion only after native command success and exact read-back.
    ///
    /// # Errors
    /// Any read or durable-publication failure leaves the outcome indeterminate.
    pub fn finish(self) -> Result<()> {
        let after = image(&self.storage, &self.ids)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| refused("receipt parent missing"))?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec(&json!({
            "format":"br-completed-effect-v1", "before":self.before, "after":after,
        }))?)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&self.path)
            .map_err(|e| BeadsError::Io(e.error))?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

/// Apply a private successor's selected effects with native transactional guards.
///
/// # Errors
/// Refuses malformed images, stale preimages, routing, or any failed native guard.
pub fn apply_file(path: &Path, cli: &config::CliOverrides) -> Result<()> {
    use std::collections::BTreeMap;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Image {
        format: String,
        ids: Vec<String>,
        records: BTreeMap<String, crate::model::Issue>,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        before: Image,
        after: Image,
    }
    if cli.db.is_none()
        || cli.no_db == Some(true)
        || cli.no_auto_import != Some(true)
        || cli.no_auto_flush != Some(true)
    {
        return Err(refused(
            "effects require explicit --db and both --no-auto flags",
        ));
    }
    let actor = cli
        .actor
        .as_deref()
        .ok_or_else(|| refused("explicit effect actor required"))?;
    let input: Input = serde_json::from_slice(&fs::read(path)?)?;
    if input.before.ids != input.after.ids
        || input.after.ids.is_empty()
        || input.after.ids.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(refused("effect selections must match and be sorted unique"));
    }
    for image in [&input.before, &input.after] {
        if image.format != "br-native-effects-v1"
            || image
                .records
                .iter()
                .any(|(id, row)| id != &row.id || !image.ids.contains(id))
        {
            return Err(refused("effect identity differs"));
        }
    }
    if input.after.records.len() != input.after.ids.len() {
        return Err(refused(
            "effect after-image must contain every selected identity",
        ));
    }
    let beads_dir = config::discover_beads_dir_with_cli(cli)?;
    if config::routing::group_issue_inputs_by_route(&input.after.ids, &beads_dir)?
        .iter()
        .any(|batch| batch.is_external)
    {
        return Err(refused("effect application requires local IDs"));
    }
    let mut ctx = config::open_storage_with_cli(&beads_dir, cli)?;
    if ctx.no_db {
        return Err(refused("effects require a database"));
    }
    let before: Vec<_> = input.before.records.into_values().collect();
    let after: Vec<_> = input.after.records.into_values().collect();
    ctx.storage.apply_record_effects(&before, &after, actor)?;
    println!("{}", json!({"verified":true, "selected":after.len()}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_receipt_identity_is_bound_to_database_and_exact_owner() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("beads.db");
        let storage = SqliteStorage::open(&db).unwrap();
        let other = temp.path().join("other.db");
        drop(SqliteStorage::open(&other).unwrap());
        let receipt = Receipt {
            path: temp.path().join("receipt.json"),
            database: db.canonicalize().unwrap(),
            ids: vec!["test-owner".into()],
            before: json!({"records":{"test-owner":{"id":"test-owner"}}}),
            storage,
        };
        assert_eq!(
            receipt.selected_id(&db, "test-owner").unwrap(),
            "test-owner"
        );
        for input in ["owner", "test-other", "TEST-OWNER", " test-owner "] {
            assert!(receipt.selected_id(&db, input).is_err());
        }
        assert!(receipt.selected_id(&other, "test-owner").is_err());
        assert!(
            receipt
                .selected_id(&temp.path().join("missing.db"), "test-owner")
                .is_err()
        );
        let mut missing_preimage = receipt;
        missing_preimage.before = json!({"records":{}});
        assert!(missing_preimage.selected_id(&db, "test-owner").is_err());
    }
}
