# Tested Scenarios and English Architecture Docs Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add tested scenarios explanations to both READMEs and create the English architecture document.

**Architecture:** Keep README.md as English primary, add README_ZH.md as Chinese counterpart, and add DOC/distributed_transaction_design_EN.md as the English architecture doc. Both READMEs include a “Tested Scenarios / 已测试场景与情况” section without scenario names, describing purpose, key checks, and expected output.

**Tech Stack:** Markdown, Mermaid

---

### Task 1: Add tested scenarios section to English README

**Files:**
- Modify: `d:/Rust/check_program/.worktrees/raft-a2/README.md`

**Step 1: Write the failing test**

Manual check: README lacks “Tested Scenarios / 已测试场景与情况”.

**Step 2: Run test to verify it fails**

Run: Manual inspection of README  
Expected: Missing tested scenarios section

**Step 3: Write minimal implementation**

Add a “Tested Scenarios / 已测试场景与情况” section with five entries (no scenario names), each containing Purpose, Key Checks, and Expected Output, aligned to verify.ps1 behavior.

**Step 4: Run test to verify it passes**

Run: Manual inspection of README  
Expected: Section present with five entries and required fields

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/README.md
git commit -m "docs: add tested scenarios to english readme"
```

---

### Task 2: Create Chinese README with tested scenarios section

**Files:**
- Create: `d:/Rust/check_program/.worktrees/raft-a2/README_ZH.md`

**Step 1: Write the failing test**

Manual check: README_ZH.md does not exist.

**Step 2: Run test to verify it fails**

Run: Manual inspection of README_ZH.md  
Expected: File missing

**Step 3: Write minimal implementation**

Create README_ZH.md as the Chinese counterpart of README.md, including “已测试场景与情况” with five entries (用途/关键检查点/输出预期).

**Step 4: Run test to verify it passes**

Run: Manual inspection of README_ZH.md  
Expected: Chinese content aligned with README.md and includes tested scenarios

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/README_ZH.md
git commit -m "docs: add chinese readme with tested scenarios"
```

---

### Task 3: Create English architecture document

**Files:**
- Create: `d:/Rust/check_program/.worktrees/raft-a2/DOC/distributed_transaction_design_EN.md`

**Step 1: Write the failing test**

Manual check: English architecture doc does not exist.

**Step 2: Run test to verify it fails**

Run: Manual inspection of distributed_transaction_design_EN.md  
Expected: File missing

**Step 3: Write minimal implementation**

Create the English architecture doc mirroring the Chinese structure with background, components, read/write path, class diagram, sequence diagram, usage, and required notes.

**Step 4: Run test to verify it passes**

Run: Manual inspection of distributed_transaction_design_EN.md  
Expected: English content present with diagrams

**Step 5: Commit**

```bash
git add d:/Rust/check_program/.worktrees/raft-a2/DOC/distributed_transaction_design_EN.md
git commit -m "docs: add english architecture document"
```
