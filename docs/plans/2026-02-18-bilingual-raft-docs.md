# Bilingual Raft Documentation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Provide one English and one Chinese document aligned to the Raft architecture, with usage/required notes and diagrams.

**Architecture:** Update the existing README to be the English primary guide, and update the existing architecture document to be the Chinese primary guide. Both documents include a class diagram and sequence diagram that reflect the current Raft-based write/read path and component responsibilities.

**Tech Stack:** Markdown, Mermaid

---

### Task 1: Update English README with Raft architecture, usage, required notes, and diagrams

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/README.md`

**Step 1: Write the failing test**

Manual check: README still describes 2PC/DualWrite/Quorum instead of Raft architecture.

**Step 2: Run test to verify it fails**

Run: Manual inspection of README  
Expected: Missing Raft overview, missing usage/required notes, missing class/sequence diagrams

**Step 3: Write minimal implementation**

Update README sections to:
- Overview, Key Features, Architecture, Directory Layout
- Usage and Required Notes (CLI args, verify.ps1 usage, schema constraints)
- Mermaid class diagram and sequence diagram
- Change log entry (2026-02-18) with reason and goal

**Step 4: Run test to verify it passes**

Run: Manual inspection of README  
Expected: Content matches Raft architecture and includes required sections and diagrams

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/README.md
git commit -m "docs: refresh english README for raft architecture"
```

---

### Task 2: Update Chinese architecture doc with Raft architecture, usage/required notes, and diagrams

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/DOC/distributed_transaction_design.md`

**Step 1: Write the failing test**

Manual check: Document still describes 2PC primary/backup instead of Raft architecture.

**Step 2: Run test to verify it fails**

Run: Manual inspection of design doc  
Expected: Missing Raft component model and diagrams

**Step 3: Write minimal implementation**

Update the document to:
- Raft-based background and goals
- Core components (Router/RaftNode/RaftStore/StateMachine/Network)
- Write path and read path details
- Usage and required notes (CLI, scripts, schema constraints)
- Mermaid class diagram and sequence diagram
- 修改记录（2026-02-18）写明原因与目的

**Step 4: Run test to verify it passes**

Run: Manual inspection of design doc  
Expected: Raft architecture content with diagrams and required notes

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/DOC/distributed_transaction_design.md
git commit -m "docs: update chinese design doc for raft architecture"
```
