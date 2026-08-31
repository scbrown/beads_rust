//! Storage-free Git hook integrations.

use crate::cli::{HookCommands, HookRunCommands, PrepareCommitMsgArgs};
use crate::error::{BeadsError, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;

const ACTOR_ENV_KEYS: [&str; 3] = ["BR_ACTOR", "BD_ACTOR", "BEADS_ACTOR"];

pub fn execute(command: &HookCommands, actor_override: Option<&str>) -> Result<()> {
    match command {
        HookCommands::Run {
            command: HookRunCommands::PrepareCommitMsg(args),
        } => execute_prepare_commit_msg(args, actor_override),
    }
}

fn execute_prepare_commit_msg(
    args: &PrepareCommitMsgArgs,
    actor_override: Option<&str>,
) -> Result<()> {
    let actor = resolve_hook_actor(actor_override);
    apply_prepare_commit_msg(args, actor.as_deref())
}

fn apply_prepare_commit_msg(args: &PrepareCommitMsgArgs, actor: Option<&str>) -> Result<()> {
    if args.source.as_deref() == Some("merge") {
        return Ok(());
    }

    let Some(actor) = actor else {
        return Ok(());
    };
    validate_actor(actor)?;

    let content = fs::read_to_string(&args.message_file).map_err(|error| {
        BeadsError::Config(format!(
            "cannot read commit message '{}': {error}",
            args.message_file.display()
        ))
    })?;
    if content.lines().any(|line| line.starts_with("Executed-By:")) {
        return Ok(());
    }

    let separator = if content.trim_end().is_empty() {
        ""
    } else if content.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let mut file = OpenOptions::new()
        .append(true)
        .open(&args.message_file)
        .map_err(|error| {
            BeadsError::Config(format!(
                "cannot update commit message '{}': {error}",
                args.message_file.display()
            ))
        })?;
    writeln!(file, "{separator}Executed-By: {actor}").map_err(|error| {
        BeadsError::Config(format!(
            "cannot update commit message '{}': {error}",
            args.message_file.display()
        ))
    })?;
    Ok(())
}

fn resolve_hook_actor(actor_override: Option<&str>) -> Option<String> {
    actor_override
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            ACTOR_ENV_KEYS.iter().find_map(|key| {
                std::env::var(key)
                    .ok()
                    .map(|actor| actor.trim().to_owned())
                    .filter(|actor| !actor.is_empty())
            })
        })
}

fn validate_actor(actor: &str) -> Result<()> {
    if actor.chars().any(char::is_control) {
        return Err(BeadsError::Validation {
            field: "actor".to_owned(),
            reason: "must be a single printable line".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn args(path: PathBuf, source: Option<&str>) -> PrepareCommitMsgArgs {
        PrepareCommitMsgArgs {
            message_file: path,
            source: source.map(str::to_owned),
            commit_object: None,
        }
    }

    #[test]
    fn appends_actor_trailer_exactly_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("COMMIT_EDITMSG");
        fs::write(&path, "subject\n").unwrap();
        let args = args(path.clone(), None);

        apply_prepare_commit_msg(&args, Some("aegis/crew/muldoon")).unwrap();
        apply_prepare_commit_msg(&args, Some("aegis/crew/muldoon")).unwrap();

        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "subject\n\nExecuted-By: aegis/crew/muldoon\n"
        );
    }

    #[test]
    fn no_actor_and_merge_source_leave_message_unchanged() {
        let dir = tempdir().unwrap();
        let no_actor = dir.path().join("NO_ACTOR");
        let merge = dir.path().join("MERGE_MSG");
        fs::write(&no_actor, "subject\n").unwrap();
        fs::write(&merge, "merge subject\n").unwrap();

        apply_prepare_commit_msg(&args(no_actor.clone(), None), None).unwrap();
        apply_prepare_commit_msg(&args(merge.clone(), Some("merge")), Some("actor")).unwrap();

        assert_eq!(fs::read_to_string(no_actor).unwrap(), "subject\n");
        assert_eq!(fs::read_to_string(merge).unwrap(), "merge subject\n");
    }

    #[test]
    fn rejects_multiline_actor() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("COMMIT_EDITMSG");
        fs::write(&path, "subject\n").unwrap();

        let result = apply_prepare_commit_msg(&args(path, None), Some("actor\nInjected: value"));

        assert!(result.is_err());
    }
}
