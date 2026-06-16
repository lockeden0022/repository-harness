# US-040 Changeset Rendering In Summary

## Status

implemented

## Lane

normal

## Product Contract

Raw JSONL changesets must be rendered into a human-readable Harness changes
section in run summaries.

## Relevant Product Docs

- `docs/SYMPHONY_SCOPE.md`

## Acceptance Criteria

- `SUMMARY.md` includes a table with operation, entity, and readable change.
- Known operations render concise descriptions.
- Unknown future operations render safely without crashing.
- The summary links to the underlying changeset path.
- Rendering is deterministic for review diffs.

## Design Notes

- Input: `.harness/changesets/<run_id>.changeset.jsonl`.
- Output: `.harness/runs/<run_id>/SUMMARY.md`.
- Depends on `US-029`.

## Validation

When updating durable proof status, use numeric booleans:
`scripts/bin/harness-cli story update --id US-040 --unit 1 --integration 1 --e2e 0 --platform 0`.

| Layer | Expected proof |
| --- | --- |
| Unit | Renderer snapshots for known and unknown operations. |
| Integration | Summary generated from fixture changeset. |
| E2E | n/a until PR creation exists. |
| Platform | n/a. |
| Release | `cargo test --workspace`; `cargo fmt --check`; `cargo clippy --workspace -- -D warnings`. |

## Harness Delta

Makes committed changesets reviewable by humans.

## Evidence

- Implemented `changeset` renderer for known and unknown semantic operations.
- `SUMMARY.md` rendering is deterministic and replaces an existing Harness
  Changes section instead of duplicating it.
- Validation: `cargo test -p harness-symphony` includes renderer and summary
  append tests; `cargo test --workspace`; `cargo fmt --check`;
  `cargo clippy --workspace -- -D warnings`.
