# US-042 Symphony Sync

## Status

implemented

## Lane

normal

## Product Contract

`harness-symphony sync` must apply committed-but-unapplied changesets to local
`harness.db` transactionally and idempotently.

## Relevant Product Docs

- `docs/SYMPHONY_SCOPE.md`

## Acceptance Criteria

- Sync scans `.harness/changesets/` in commit order.
- Sync compares against applied changeset records in `.symphony/state.db` and
  `harness.db`.
- Every unapplied changeset is replayed through `harness-cli`.
- Running sync twice is safe and skips already applied changesets.
- On failure, sync reports the changeset and operation, leaves `harness.db`
  intact, and remains safe to re-run.
- `status` and `doctor` warn when committed changesets are unapplied.

## Design Notes

- Command: `harness-symphony sync`.
- Depends on `US-030`.
- Fresh clone rebuild may delegate to `US-031`.

## Validation

When updating durable proof status, use numeric booleans:
`scripts/bin/harness-cli story update --id US-042 --unit 1 --integration 1 --e2e 1 --platform 1`.

| Layer | Expected proof |
| --- | --- |
| Unit | Applied/unapplied detection and failure classification. |
| Integration | Fixture changesets apply once, skip on second run, rollback on malformed operation. |
| E2E | Simulate merge by adding committed changeset fixture, run sync, query updated DB. |
| Platform | macOS/Linux shell smoke. |
| Release | `cargo test --workspace`; `cargo fmt --check`; `cargo clippy --workspace -- -D warnings`. |

## Harness Delta

Replaces per-PR reconciliation with idempotent local sync.

## Evidence

- Added `harness-symphony sync` over committed `.harness/changesets/*.jsonl`.
- Sync uses `scripts/bin/harness-cli db changeset apply`, records local
  changeset sync state, and is safe to rerun.
- `status` and `doctor` warn when committed changesets are unapplied.
- Smoke: temporary destination repo reported one unapplied changeset, sync
  applied it and created story `US-SYNC`, and a second sync skipped it.
- Validation: `cargo test --workspace`; `cargo fmt --check`;
  `cargo clippy --workspace -- -D warnings`.
