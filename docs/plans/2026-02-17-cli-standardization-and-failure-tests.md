# CLI 标准化与故障用例补充 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 标准化 CLI 入口，新增三类故障与一致性用例，并将数据源配置做成可选项后完成验证。

**Architecture:** 以 CLI 作为统一入口，将场景驱动逻辑集中在 verify.ps1；在 client/coordinator 增加可控的阶段停顿以支持故障注入；server 保持引擎参数可选但默认 sqlite。

**Tech Stack:** Rust (tokio, tonic), PowerShell verify.ps1, SQLite (rusqlite)

---

### Task 1: 定义与落地统一 CLI 入口规范

**Files:**
- Modify: `d:\Rust\check_program\src\bin\server.rs`
- Modify: `d:\Rust\check_program\src\bin\client.rs`
- Modify: `d:\Rust\check_program\README.md`

**Step 1: Write the failing test**

在 `verify.ps1` 中调用预期的 CLI 参数（例如 `--engine`、`--pause-before-commit-ms`、`--scenario`），当前应失败或不支持。

**Step 2: Run test to verify it fails**

Run: `./verify.ps1 -Scenario prepare_commit_kill`
Expected: 失败，提示未知参数或无法触发故障注入。

**Step 3: Write minimal implementation**

- 在 `server.rs` 明确保留 `--engine` 可选参数，默认 sqlite。
- 在 `client.rs` 增加统一参数输入入口，支持后续测试场景参数透传。
- 在 `README.md` 增加 CLI 参数说明与标准化入口规范说明。

**Step 4: Run test to verify it passes**

Run: `./verify.ps1 -Scenario prepare_commit_kill`
Expected: 进入指定场景并开始执行（即使后续故障注入未完成）。

**Step 5: Commit**

```bash
git add src/bin/server.rs src/bin/client.rs README.md verify.ps1
git commit -m "feat: standardize cli entrypoints"
```

---

### Task 2: 新增三类故障/一致性用例（脚本驱动）

**Files:**
- Modify: `d:\Rust\check_program\verify.ps1`
- Modify: `d:\Rust\check_program\src\coordinator.rs`

**Step 1: Write the failing test**

在 `verify.ps1` 中加入三个场景并执行：
- `restart_single_node`
- `prepare_commit_kill`
- `repeat_verify_3x`

Expected: 目前无法执行完整流程或无法注入故障。

**Step 2: Run test to verify it fails**

Run: `./verify.ps1 -Scenario restart_single_node`
Expected: 脚本未能完成场景或输出缺失。

**Step 3: Write minimal implementation**

- `verify.ps1` 增加 `-Scenario` 参数并实现三种场景流程。
- `coordinator.rs` 新增可选的“提交前暂停”配置（例如环境变量/CLI 参数透传），让脚本可以在 Prepare 与 Commit 之间触发节点宕机。

**Step 4: Run test to verify it passes**

Run:
- `./verify.ps1 -Scenario restart_single_node`
- `./verify.ps1 -Scenario prepare_commit_kill`
- `./verify.ps1 -Scenario repeat_verify_3x`
Expected: 三种场景均能跑通并输出一致性结果。

**Step 5: Commit**

```bash
git add verify.ps1 src/coordinator.rs src/bin/client.rs
git commit -m "test: add failure scenarios and pause hook"
```

---

### Task 3: 数据源作为可选配置并标准化入口说明

**Files:**
- Modify: `d:\Rust\check_program\README.md`
- Modify: `d:\Rust\check_program\DOC\distributed_transaction_design.md`

**Step 1: Write the failing test**

验证文档中没有“可选数据源配置与标准化入口”说明。

**Step 2: Run test to verify it fails**

Run: 人工检查文档是否缺失该说明
Expected: 缺失

**Step 3: Write minimal implementation**

- README：补充 CLI 标准化入口与 `--engine` 可选配置说明
- 设计文档：补充接入数据源步骤与一致性要求

**Step 4: Run test to verify it passes**

Run: 人工检查文档
Expected: 说明完整

**Step 5: Commit**

```bash
git add README.md DOC/distributed_transaction_design.md
git commit -m "docs: clarify optional engine and entrypoint"
```

---

### Task 4: 全量验证

**Files:**
- Test: `d:\Rust\check_program\verify.ps1`

**Step 1: Run verification**

Run: `./verify.ps1 -Scenario restart_single_node`
Run: `./verify.ps1 -Scenario prepare_commit_kill`
Run: `./verify.ps1 -Scenario repeat_verify_3x`
Expected: 全部通过，输出一致性校验结果

**Step 2: Commit**

```bash
git status
git commit -m "test: verify scenarios" || true
```
