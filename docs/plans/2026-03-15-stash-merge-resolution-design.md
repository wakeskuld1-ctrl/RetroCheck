# Stash Merge Resolution Design (2026-03-15)

## Context
- Goal: resolve stash pop conflicts on `master` while preserving specific stash additions.
- Constraints: keep master versions for edge schema/gateway/session files; keep stash additions for audit event and extra tests; include all untracked files in the next commit.

## Options Considered
### Option A (recommended): manual, file-by-file merge
**Pros**
- Exact control over which side to keep per file.
- Lowest risk of losing stash-only tests or audit event.
**Cons**
- More manual steps.

### Option B: `--ours/--theirs` fast resolution + patching
**Pros**
- Fastest.
**Cons**
- High risk of missing stash-only test/audit changes.

### Option C: apply stash on temporary branch and cherry-pick
**Pros**
- Clean commit boundaries.
**Cons**
- More workflow complexity, may still require conflicts.

## Selected Approach
Use Option A and resolve conflicts per file with explicit keep rules.

## Resolution Rules
- Keep master for:
  - `src/hub/edge_gateway.rs`
  - `src/hub/edge_schema.rs`
  - `src/hub/edge_session_schema.rs`
- Merge master + stash for:
  - `src/management/cluster_admin_service.rs` (retain master logic and add `append_audit_event`)
  - `tests/cluster_admin_add_hub.rs` (retain master test and add stash tests)
- Include all untracked files (DOC/, docs/, promo/, *.bak, *_outer.rs) in the commit.

## Verification
- Preferred: `cargo test -- --test-threads=1`
- Minimum: run tests related to `tests/cluster_admin_add_hub.rs` if full suite is too slow.
