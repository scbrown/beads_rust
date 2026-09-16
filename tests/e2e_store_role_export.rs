//! E2E coverage for the `store.role = export` write gate (aegis-6yksbj).
//!
//! WHAT THIS PROTECTS. `.beads/issues.jsonl` is tracked in git; `.beads/redirect` holds an
//! absolute local path and is ignored. So git ships the thing that makes a directory look like a
//! store and withholds the thing that says otherwise, and a fresh clone, a `git clean -xfd` and a
//! new worktree all arrive in a state where the first write mints a local database and puts the
//! record where nobody looks. A marker IN the export travels with it; an environment variable
//! cannot — not to a laptop checkout, not to a cron.
//!
//! THE CONTROL ARM IS THE POINT. Every refusal test here is paired with the same command against
//! the same workspace WITHOUT the marker, which must still succeed. Without that pair, a test
//! that refuses because the workspace was broken in some unrelated way passes just as happily as
//! one that refuses for the declared reason.

mod common;

use common::cli::{BrWorkspace, run_br};
use std::fs;

/// Initialize a workspace, then optionally declare it an export.
fn workspace_with_role(role: Option<&str>) -> BrWorkspace {
    let ws = BrWorkspace::new();
    let init = run_br(&ws, ["init", "--prefix", "tst"], "init");
    assert!(init.status.success(), "init failed: {}", init.stderr);
    if let Some(role) = role {
        let config = ws.root.join(".beads").join("config.yaml");
        fs::write(
            &config,
            format!("store:\n  role: {role}\n  authority: \"rig:/somewhere/else/_beads\"\n"),
        )
        .expect("write config.yaml");
    }
    ws
}

#[test]
fn export_marker_refuses_create_and_control_still_creates() {
    // ARM 1: the marker is present -> refuse, name the authority, mint nothing.
    let exported = workspace_with_role(Some("export"));
    let refused = run_br(&exported, ["create", "should not exist"], "create-export");
    assert!(
        !refused.status.success(),
        "create SUCCEEDED against an export workspace; stdout={}",
        refused.stdout
    );
    let combined = format!("{}{}", refused.stdout, refused.stderr);
    assert!(
        combined.contains("store.role") || combined.contains("export"),
        "refusal does not say WHY: {combined}"
    );
    assert!(
        combined.contains("/somewhere/else/_beads"),
        "refusal does not name the authority, so the reader cannot act on it: {combined}"
    );

    // ARM 2 (CONTROL): same command, same shape of workspace, no marker -> must still work.
    // This is what makes arm 1 evidence about the marker rather than about br being broken.
    let ordinary = workspace_with_role(None);
    let created = run_br(&ordinary, ["create", "ordinary workspace"], "create-control");
    assert!(
        created.status.success(),
        "CONTROL FAILED: create refused without the marker, so arm 1 proves nothing: {}",
        created.stderr
    );
}

#[test]
fn export_marker_leaves_reads_working() {
    // A refusal that also blocks reads would push people to delete the marker.
    let exported = workspace_with_role(Some("export"));
    let listed = run_br(&exported, ["list"], "list-export");
    assert!(
        listed.status.success(),
        "list was refused against an export workspace; reads must stay available: {}",
        listed.stderr
    );
}

#[test]
fn export_marker_refuses_init_which_is_how_the_local_store_gets_minted() {
    // `br init` is absent from `is_mutating_command` and present in the broader predicate. It is
    // also the single command that turns an export directory into the local store this gate
    // exists to prevent, so gating on the narrow predicate alone would refuse `create` while
    // permitting the command that makes `create` succeed.
    let exported = workspace_with_role(Some("export"));
    let reinit = run_br(&exported, ["init", "--prefix", "other"], "init-export");
    assert!(
        !reinit.status.success(),
        "init SUCCEEDED against an export workspace — the gate is narrower than the hazard"
    );
}

#[test]
fn unknown_role_value_does_not_refuse() {
    // Only the literal `export` gates. A typo or a future role must not silently brick a
    // workspace: fail-open on an unrecognized value, because the cost of a wrong refusal here is
    // an agent that cannot write anything and no obvious cause.
    let odd = workspace_with_role(Some("expart"));
    let created = run_br(&odd, ["create", "typo in role"], "create-typo-role");
    assert!(
        created.status.success(),
        "an unrecognized store.role value refused a write; it must fail open: {}",
        created.stderr
    );
}

#[test]
fn role_store_is_explicitly_permitted_so_the_cutover_flip_is_testable() {
    // At cutover the marker flips to `role = "store"` in one commit. That flip must be a working
    // workspace, not merely a non-export one, or the cutover is untested until it happens.
    let flipped = workspace_with_role(Some("store"));
    let created = run_br(&flipped, ["create", "after the cutover flip"], "create-role-store");
    assert!(
        created.status.success(),
        "role=store refused a write, so the cutover flip would not work: {}",
        created.stderr
    );
}
