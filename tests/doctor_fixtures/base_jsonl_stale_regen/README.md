# base_jsonl_stale_regen

This historical fixture now proves preservation. A regular merge ancestor
normally predates the local JSONL and can have different bytes. Replacing it
with local state causes a later three-way merge to discard local edits.

`base_jsonl` must report this ancestor as valid. `doctor --repair` and undo
of unrelated repairs must preserve its exact bytes. No
`doctor.base_jsonl_regen` write may appear in the repair journal.

Unsafe symlink handling remains covered by `base_jsonl_symlink_quarantine`;
missing-anchor initialization remains covered by `base_jsonl_missing_post_flush`.
