# Stash Merge Resolution Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Resolve stash conflicts on `master` while preserving specified stash additions and committing all restored untracked files.

**Architecture:** Manual, file-level conflict resolution with explicit keep rules; then stage all changes and commit.

**Tech Stack:** Git, Rust codebase, PowerShell.

---

### Task 1: Confirm current conflict set and keep rules

**Files:**
- Modify: `d:\Rust\check_program\src\hub\edge_gateway.rs`
- Modify: `d:\Rust\check_program\src\hub\edge_schema.rs`
- Modify: `d:\Rust\check_program\src\hub\edge_session_schema.rs`
- Modify: `d:\Rust\check_program\src\management\cluster_admin_service.rs`
- Modify: `d:\Rust\check_program\tests\cluster_admin_add_hub.rs`

**Step 1: List conflicts**

Run: `git status -sb`  
Expected: 5 `UU` entries and untracked files.

**Step 2: Re-state keep rules in notes**

Action: Note explicit keep rules for each file before editing.

---

### Task 2: Resolve conflicts by file

**Files:**
- Modify: `d:\Rust\check_program\src\hub\edge_gateway.rs`
- Modify: `d:\Rust\check_program\src\hub\edge_schema.rs`
- Modify: `d:\Rust\check_program\src\hub\edge_session_schema.rs`
- Modify: `d:\Rust\check_program\src\management\cluster_admin_service.rs`
- Modify: `d:\Rust\check_program\tests\cluster_admin_add_hub.rs`

**Step 1: Keep master for edge gateway/schema/session**

Action: Accept master version for three files and remove conflict markers.

**Step 2: Merge `cluster_admin_service.rs`**

Action: Keep master logic for leader redirect and add `append_audit_event` from stash.

**Step 3: Merge `cluster_admin_add_hub.rs`**

Action: Keep master test and include stash remove_hub/add_edge tests.

**Step 4: Add inline change notes**

Action: Add per-change Markdown notes with date, reason, and purpose near edited code.

---

### Task 3: Stage all changes including untracked files

**Files:**
- Add: `d:\Rust\check_program\DOC\code_audit_todo_2026-03-13.md`
- Add: `d:\Rust\check_program\DOC\unified_sql_standard_spec_zh.md`
- Add: `d:\Rust\check_program\docs\changes\2026-03-09-weak-network-low-power-baseline.md`
- Add: `d:\Rust\check_program\docs\plans\2026-03-09-farm-appendix-gap-closure-plan.md`
- Add: `d:\Rust\check_program\docs\plans\2026-03-11-unified-sql-and-node-join-status-plan.md`
- Add: `d:\Rust\check_program\promo\*`
- Add: `d:\Rust\check_program\src\management\cluster_admin_service.rs.bak`
- Add: `d:\Rust\check_program\src\management\cluster_admin_service_outer.rs`

**Step 1: Stage all**

Run: `git add .`  
Expected: no unmerged paths, untracked files added.

---

### Task 4: Verify and commit

**Files:**
- Modify/Add: all staged changes

**Step 1: Minimal verification**

Run: `cargo test -- --test-threads=1`  
Expected: PASS (or capture failures for debugging).

**Step 2: Commit**

Run: `git commit -m "chore: resolve stash conflicts and retain audit/tests"`  
Expected: new commit with conflict resolution and added files.

---

### Task 5: Post-task journal

**Files:**
- Modify: `d:\Rust\check_program\.trae\CHANGELOG_TASK.md`

**Step 1: Append task entry**

Action: Use `task-journal` skill after completion.
