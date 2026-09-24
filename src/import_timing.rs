//! Nanosecond accumulators for the JSONL import hot path (aegis-q3q97d).
//!
//! `br`'s first read in a clone with no database pays a whole JSONL import, and that
//! cost is asymptotically QUADRATIC in the record count: the per-doubling exponent
//! RISES with N (1.56 -> 1.81 -> 1.84 for the issue insert, 1.56 -> 2.10 -> 1.96 for
//! the relation inserts across 2k/4k/8k/16k), so it is not a fixed power law that a
//! faster machine amortises away.
//!
//! The level-2 profile attributed 99.9% of `process_import_action` to exactly two
//! callees — `insert_new_import_issue` (33%) and `insert_new_issue_relations_for_import_in_tx`
//! (67%) — and refuted the three plausible SQLite-side explanations by control
//! (page cache: 32x larger cache moves the stream 2%; a synthetic real-SQLite
//! workload of the same shape is LINEAR at both cache sizes and ~43x faster;
//! `applied_issues.push` is flat at 0.1%). This module carries the accumulators one
//! level further down, into the individual statements, so the next cut is measured
//! rather than argued.
//!
//! # Why these are statics
//!
//! The instrumented calls span two modules (`sync` drives the loop, `storage` owns
//! the statements) and sit on signatures shared with non-import callers. Threading a
//! timing context through would change those signatures and their call sites; a
//! module of statics keeps the instrument additive.
//!
//! # Cost when disabled
//!
//! [`mark`] is one `Relaxed` atomic load that returns `None`, and [`add`] is then a
//! no-op on `None`. Nothing is read from the environment per record, nothing is
//! formatted, and no timer is started. Arming happens once per import, in
//! `ImportPhaseTimer::new`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Armed once per import from `BR_IMPORT_TIMING`; read once per instrumented call.
static ARMED: AtomicBool = AtomicBool::new(false);

/// One accumulator, in nanoseconds, plus the label the report prints for it.
pub(crate) struct Substep {
    label: &'static str,
    nanos: AtomicU64,
}

impl Substep {
    const fn new(label: &'static str) -> Self {
        Self {
            label,
            nanos: AtomicU64::new(0),
        }
    }

    /// The label this accumulator is reported under.
    pub(crate) fn label(&self) -> &'static str {
        self.label
    }

    /// Accumulated nanoseconds.
    pub(crate) fn nanos(&self) -> u128 {
        u128::from(self.nanos.load(Ordering::Relaxed))
    }

    fn reset(&self) {
        self.nanos.store(0, Ordering::Relaxed);
    }
}

// ---- Level 2: inside `process_import_action` (driven from `sync`) ----------------

pub(crate) static INSERT_ISSUE: Substep = Substep::new("insert_new_import_issue");
pub(crate) static HAS_OWNED_RELATIONS: Substep =
    Substep::new("has_owned_relation_rows_for_import");
pub(crate) static INSERT_RELATIONS: Substep =
    Substep::new("insert_new_issue_relations_for_import_in_tx");
pub(crate) static SYNC_RELATIONS: Substep = Substep::new("sync_issue_relations");
pub(crate) static APPLIED_PUSH: Substep = Substep::new("applied_issues.push(clone)");

// ---- Level 3: inside those two, i.e. the statements themselves (owned by `storage`)

/// `ImportIssueTimestampStrings::from_issue` — pure formatting, no database.
pub(crate) static ISSUE_TIMESTAMPS: Substep = Substep::new("| timestamps (no db)");
/// The single `INSERT INTO issues` row write.
pub(crate) static ISSUE_ROW: Substep = Substep::new("| insert_issue_row_for_import");
/// Label validation plus one `INSERT OR IGNORE` per unique label.
pub(crate) static INSERT_LABELS: Substep = Substep::new("| insert_labels_for_import");
/// Dependency validation plus one insert per unique dependency.
pub(crate) static INSERT_DEPENDENCIES: Substep = Substep::new("| insert_dependencies_for_import");
/// Comment validation plus one insert per comment.
pub(crate) static INSERT_COMMENTS: Substep = Substep::new("| insert_comments_for_import");

/// Every accumulator, in report order. Level 3 entries are indented by their label.
pub(crate) const ALL: &[&Substep] = &[
    &INSERT_ISSUE,
    &ISSUE_TIMESTAMPS,
    &ISSUE_ROW,
    &HAS_OWNED_RELATIONS,
    &INSERT_RELATIONS,
    &INSERT_LABELS,
    &INSERT_DEPENDENCIES,
    &INSERT_COMMENTS,
    &SYNC_RELATIONS,
    &APPLIED_PUSH,
];

/// Arm or disarm timing and zero every accumulator.
///
/// Called once per import so that a second import in the same process reports its
/// own numbers rather than the sum of both.
pub(crate) fn arm(enabled: bool) {
    ARMED.store(enabled, Ordering::Relaxed);
    for substep in ALL {
        substep.reset();
    }
}

/// `Some(now)` only while timing is armed.
pub(crate) fn mark() -> Option<Instant> {
    ARMED.load(Ordering::Relaxed).then(Instant::now)
}

/// Add the time elapsed since `started` to `substep`. A no-op when timing is off.
pub(crate) fn add(substep: &Substep, started: Option<Instant>) {
    if let Some(started) = started {
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        substep.nanos.fetch_add(nanos, Ordering::Relaxed);
    }
}
