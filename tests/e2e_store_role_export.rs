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
    let created = run_br(
        &ordinary,
        ["create", "ordinary workspace"],
        "create-control",
    );
    assert!(
        created.status.success(),
        "CONTROL FAILED: create refused without the marker, so arm 1 proves nothing: {}",
        created.stderr
    );
}

#[test]
fn export_marker_leaves_reads_working_when_a_local_database_already_exists() {
    // A refusal that also blocks reads would push people to delete the marker.
    //
    // ⚠ READ THE FIXTURE BEFORE READING THIS TEST'S NAME. `workspace_with_role` runs
    // `br init` and THEN writes the marker, so this workspace HAS a beads.db. That is the
    // only case this test covers, and until aegis-prtadm the name claimed the general one.
    // The case it did not cover — export role with NO local database — is the one that
    // auto-imported 16,987 records into a freshly minted store and burned 80s of CPU on a
    // plain `br list`. It is covered by the test below, with its own control arm.
    let exported = workspace_with_role(Some("export"));
    assert!(
        exported.root.join(".beads").join("beads.db").exists(),
        "fixture changed: this test is specifically about the db-already-present case"
    );
    let listed = run_br(&exported, ["list"], "list-export");
    assert!(
        listed.status.success(),
        "list was refused against an export workspace that already has a local database; \
         refusing there would break recovery for anyone who minted one before the read gate: {}",
        listed.stderr
    );
}

/// An export workspace as a fresh CLONE arrives: tracked config.yaml + issues.jsonl,
/// and no database, because `.beads/beads.db` is ignored by git.
fn cloned_export_workspace(role: Option<&str>) -> BrWorkspace {
    let ws = BrWorkspace::new();
    let beads = ws.root.join(".beads");
    fs::create_dir_all(&beads).expect("create .beads");
    let mut config = String::from("no-auto-import: false\n");
    if let Some(role) = role {
        config.push_str(&format!(
            "store:\n  role: {role}\n  authority: \"rig:/somewhere/else/_beads\"\n"
        ));
    }
    fs::write(beads.join("config.yaml"), config).expect("write config.yaml");
    fs::write(
        beads.join("issues.jsonl"),
        "{\"id\":\"tst-aaa\",\"title\":\"from the export\",\"status\":\"open\",\
         \"issue_type\":\"task\",\"priority\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\
         \"updated_at\":\"2026-01-01T00:00:00Z\"}\n",
    )
    .expect("write issues.jsonl");
    ws
}

#[test]
fn export_marker_refuses_a_read_that_would_mint_and_control_still_mints() {
    // aegis-prtadm. The write gate did not cover this: a READ in an export clone fell
    // straight through into auto-import. Measured on a real clone of aegis.git carrying the
    // 16,987-record export, before this gate: `br list --limit 3` -> 80s of CPU and an
    // 8-file local store; `br show <id>` -> the same. The marker guarded `create` only.

    // ARM 1: marker present, no local database -> refuse, and mint NOTHING.
    let exported = cloned_export_workspace(Some("export"));
    let refused = run_br(&exported, ["list"], "list-export-clone");
    assert!(
        !refused.status.success(),
        "a read against a marked export clone SUCCEEDED — it auto-imported and minted"
    );
    assert!(
        refused.stderr.contains("store.role = export"),
        "refusal did not name the marker: {}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("/somewhere/else/_beads"),
        "refusal did not point at the authority, so the reader has nowhere to go: {}",
        refused.stderr
    );
    assert!(
        !exported.root.join(".beads").join("beads.db").exists(),
        "REFUSED AND MINTED ANYWAY — the refusal is cosmetic"
    );

    // ARM 2 (CONTROL): the same workspace WITHOUT the marker must still import and mint.
    // Without this arm, a refusal caused by the workspace being broken in some unrelated
    // way passes exactly as happily as one caused by the marker.
    let plain = cloned_export_workspace(None);
    let listed = run_br(&plain, ["list"], "list-plain-clone");
    assert!(
        listed.status.success(),
        "CONTROL FAILED: an unmarked clone could not be read, so arm 1 proves nothing: {}",
        listed.stderr
    );
    assert!(
        plain.root.join(".beads").join("beads.db").exists(),
        "CONTROL FAILED: the unmarked clone minted nothing, so arm 1's 'no beads.db' is vacuous"
    );
}

#[test]
fn export_marker_does_not_refuse_an_explicit_db_pointing_somewhere_else() {
    // The refusal text tells the reader to run `br --db <that path> <your command>`. If the
    // gate then refused exactly that, the marker would look broken to the one person doing
    // what it asked, and the obvious next move is to delete the marker.
    //
    // The gate therefore fires only when the database that WOULD be minted is inside THIS
    // workspace's .beads. Proven with a real elsewhere-store rather than a bare assertion:
    // create one, then read it from inside the marked export clone.
    let elsewhere = workspace_with_role(None);
    let init_ok = run_br(&elsewhere, ["create", "a real bead"], "create-elsewhere");
    assert!(
        init_ok.status.success(),
        "fixture failed: could not create the elsewhere store: {}",
        init_ok.stderr
    );
    let elsewhere_db = elsewhere.root.join(".beads").join("beads.db");
    assert!(elsewhere_db.exists(), "fixture failed: no elsewhere db");

    let exported = cloned_export_workspace(Some("export"));
    let listed = run_br(
        &exported,
        ["--db", elsewhere_db.to_str().expect("utf8 path"), "list"],
        "list-export-explicit-db",
    );
    assert!(
        listed.status.success(),
        "an explicit --db at another store was REFUSED from inside an export clone — that is \
         the escape the refusal text recommends: {}",
        listed.stderr
    );
    assert!(
        !exported.root.join(".beads").join("beads.db").exists(),
        "reading another store minted one in the export clone"
    );
}

#[test]
fn export_marker_does_not_gag_doctor_which_reports_the_missing_database() {
    // doctor is deliberately outside the read gate: reporting a missing database IS its job,
    // and it does not mint. Refusing it would remove the one command that can explain the
    // refusal to whoever hit it.
    let exported = cloned_export_workspace(Some("export"));
    let doctored = run_br(&exported, ["doctor"], "doctor-export-clone");
    assert!(
        doctored.stdout.contains("HEALTH") || doctored.stdout.contains("db.exists"),
        "doctor produced no findings against an export clone: {} {}",
        doctored.stdout,
        doctored.stderr
    );
    assert!(
        !exported.root.join(".beads").join("beads.db").exists(),
        "doctor minted a database"
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
    let created = run_br(
        &flipped,
        ["create", "after the cutover flip"],
        "create-role-store",
    );
    assert!(
        created.status.success(),
        "role=store refused a write, so the cutover flip would not work: {}",
        created.stderr
    );
}
