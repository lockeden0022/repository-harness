# US-041 Optional PR Creation

## Status

implemented

## Lane

normal

## Product Contract

Symphony may create pull requests for run artifacts and code changes, but PR
creation must be configurable and outcome-aware.

## Relevant Product Docs

- `docs/SYMPHONY_SCOPE.md`

## Acceptance Criteria

- PR creation supports `create: ask` and disabled modes.
- Completed implementation runs open normal PRs when enabled.
- Blocked, `needs_intake`, and partial runs open draft PRs only when useful
  artifacts exist and policy allows it.
- Failed and cancelled runs do not open PRs by default.
- PRs include summary, result, and changeset artifacts.
- PRs never include `harness.db` or `.symphony/` files.

## Design Notes

- Suggested commands: `harness-symphony pr create <run_id>` and `pr retry`.
- Provider: GitHub can be first; keep provider behind an adapter.
- Use config under `pull_request`.

## Validation

When updating durable proof status, use numeric booleans:
`scripts/bin/harness-cli story update --id US-041 --unit 1 --integration 1 --e2e 1 --platform 1`.

| Layer | Expected proof |
| --- | --- |
| Unit | PR policy decisions by outcome/config. |
| Integration | Fake PR adapter receives expected title/body/files. |
| E2E | Optional manual GitHub smoke or documented skip if credentials unavailable. |
| Platform | Git staging check excludes forbidden files. |
| Release | `cargo test --workspace`; `cargo fmt --check`; `cargo clippy --workspace -- -D warnings`. |

## Harness Delta

Connects local Symphony output to team review while keeping PR creation optional.

## Evidence

- Added `harness-symphony pr create <run_id>` and `pr retry <run_id>` with
  `--dry-run` support.
- PR planning respects disabled policy, completed normal PRs, configured draft
  outcomes, and failed/cancelled default refusal.
- PR artifacts include `SUMMARY.md`, `RESULT.json`, and the semantic changeset;
  forbidden staged `harness.db` and `.symphony/` paths are rejected.
- Validation: PR policy unit tests; `cargo test --workspace`;
  `cargo fmt --check`; `cargo clippy --workspace -- -D warnings`.
