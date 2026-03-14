# Unified SQL 与节点加入策略实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 明确“继续执行 SQL 主线”与“节点加入策略”的当前状态，避免后续遗忘与执行偏移。

**Architecture:** 以统一 SQL 路由校验为主线推进；Cluster 管理侧采用“先定语义、后实现”的策略。`addhub` 语义已确认，`addedge` 暂不实现，仅冻结两场景规则与状态。

**Tech Stack:** Rust、OpenRaft、SQLite、Tonic gRPC、Clap CLI

---

## 状态图例
- `✅ 已确认`：需求和策略已定，不再反复讨论
- `🟡 进行中`：当前正在推进
- `⏳ 待实现`：已定义但尚未编码
- `🚫 暂不执行`：暂时冻结，不纳入当前迭代

## 冻结决策（防遗忘）
- `✅ 已确认`：`addhub` 继续沿用现有命令与语义
- `✅ 已确认`：`addedge` 需要区分两种场景
  - 场景 A：独立新节点（不归组）→ 不拉元数据
  - 场景 B：加入现有分组（带 group_id）→ 需要拉组元数据做初始化
- `✅ 已确认`：当前迭代优先继续统一 SQL 主线，不开工 `addedge` 实现

## 当前执行计划（按优先级）

### P0 统一 SQL 主线
- `✅ 已确认`：`FOR EDGE(device_id=...)` 目标存在性校验已补齐（与 group 校验对称）
- `✅ 已确认`：`write_batch` 在 device/group 路由下的一致性错误行为覆盖已补齐
- `✅ 已确认`：系统表缺失与目标不存在的错误语义边界已完成拆分，并覆盖 follower 转发透传

### P1 节点加入语义沉淀（最小实现完成，不改 proto/CLI）
- `✅ 已确认`：AddEdge 语义采用“双场景分流”
- `✅ 已完成`：不修改 proto、不新增 CLI 参数；在 `cluster_admin_service` 内完成可注入 Provider + 可注入 forward target 的最小语义实现
- `✅ 已确认`：首批失败用例命名、断言边界与最小回归矩阵已冻结并已落地测试

## P1 预研拆解（已从计划推进到最小实现）
- `✅ 已确认`：场景 A（独立新节点）首条失败用例命名为 `addedge_standalone_does_not_pull_group_metadata`，核心断言为“不触发组元数据拉取”。
- `✅ 已确认`：场景 B（加入已有分组）首条失败用例命名为 `addedge_with_group_id_pulls_group_metadata_before_ready`，核心断言为“先拉取元数据，再标记 ready”。
- `✅ 已确认`：两场景共用错误语义基线：元数据缺失与目标不存在需分开编码，不复用单一失败码。
- `✅ 已完成`：四条首批用例已落地并转绿（场景 A 成功、场景 B 成功、`TARGET_METADATA_MISSING`、`TARGET_NOT_FOUND`）。
- `✅ 已完成`：follower 转发路径 AddEdge 语义透传回归已补齐（Matrix-5/Matrix-6），保持不改 proto/CLI。

## 开工入口（下次直接接）
- 当前迭代（仅计划）：`docs/plans/2026-03-11-unified-sql-and-node-join-status-plan.md`
- 下阶段立项（AddEdge TDD）测试入口：`tests/cluster_admin_add_hub.rs`
- 下阶段立项（AddEdge TDD）实现入口：`src/management/cluster_admin_service.rs`
- 首批失败测试名（已冻结）：
  - `addedge_standalone_does_not_pull_group_metadata`
  - `addedge_with_group_id_pulls_group_metadata_before_ready`
- 立项后的最小回归命令：
  - `cargo test --test cluster_admin_add_hub -- --nocapture`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo check --all-targets --all-features`

## P1 立项入口与验证门禁（仅计划，不编码）
- `✅ 已确认`：立项触发条件（全部满足才开工）
  - `Gate-1`：失败用例先落地，且文件内可直接检索到两个冻结测试名
  - `Gate-2`：场景 A/B 都先红灯（失败），禁止先写实现再补测试
  - `Gate-3`：错误语义断言必须区分 `TARGET_METADATA_MISSING` 与 `TARGET_NOT_FOUND`
- `✅ 已确认`：立项执行顺序（禁止跳步）
  - `Step-1`：在 `tests/cluster_admin_add_hub.rs` 写入首批失败用例（A/B）
  - `Step-2`：只在 `src/management/cluster_admin_service.rs` 做最小实现，直到 A/B 首次转绿
  - `Step-3`：补 follower 转发与单节点回归，再执行全量静态检查
- `✅ 已确认`：立项验收门禁（全部通过才允许关闭任务）
  - `Check-1`：`cargo test --test cluster_admin_add_hub -- --nocapture` 通过
  - `Check-2`：`cargo clippy --all-targets --all-features -- -D warnings` 通过
  - `Check-3`：`cargo check --all-targets --all-features` 通过
  - `Check-4`：状态变更记录补充“为何改这里而不是改其他入口”的一句话说明

## P1 首批失败测试断言模板（仅计划，不编码）
- `✅ 已确认`：`addedge_standalone_does_not_pull_group_metadata`（场景 A）断言模板
  - `Assert-A1`：调用 AddEdge（不带 group_id）后，组元数据拉取调用计数为 0
  - `Assert-A2`：节点状态允许进入 ready，但不依赖 `sys_edge_group` 初始化路径
  - `Assert-A3`：若误触发组拉取，应明确失败并输出“standalone should not pull group metadata”
- `✅ 已确认`：`addedge_with_group_id_pulls_group_metadata_before_ready`（场景 B）断言模板
  - `Assert-B1`：调用 AddEdge（带 group_id）后，先发生组元数据拉取，再允许 ready
  - `Assert-B2`：当组元数据缺失时返回 `TARGET_METADATA_MISSING`，而非 `TARGET_NOT_FOUND`
  - `Assert-B3`：当 group_id 不存在时返回 `TARGET_NOT_FOUND`，不与缺元数据混码

## P1 回归矩阵门禁（立项后执行）
- `✅ 已确认`：最小矩阵（必须全绿）
  - `Matrix-1`：单节点 + 场景 A（不带 group_id）+ 成功路径
  - `Matrix-2`：单节点 + 场景 B（带 group_id）+ 成功路径
  - `Matrix-3`：单节点 + 场景 B + `TARGET_METADATA_MISSING` 失败路径
  - `Matrix-4`：单节点 + 场景 B + `TARGET_NOT_FOUND` 失败路径
- `✅ 已确认`：扩展矩阵（最小矩阵通过后执行）
  - `Matrix-5`：follower 转发 + 场景 B + `TARGET_METADATA_MISSING` 透传
  - `Matrix-6`：follower 转发 + 场景 B + `TARGET_NOT_FOUND` 透传

## 完成标准（本计划对应）
- 本文档可作为唯一“当前共识”入口，能回答“现在做什么、先不做什么、为什么”。
- 后续每次状态变化仅在本文件追加状态变更，不回滚已确认决策。

## 状态变更记录
- `2026-03-11 22:40`：`FOR EDGE(device_id=...)` 目标不存在回归测试已补齐（`write` + `write_batch`），对应 P0 第 1 项由“进行中”推进为“已完成（测试覆盖）”。
- `2026-03-11 22:40`：P0 第 2 项“write_batch 在 device/group 路由下的一致性错误行为覆盖”由“待实现”推进为“进行中”，已覆盖 device-not-found 批量失败场景，后续补 group 及更细错误边界。
- `2026-03-11 22:52`：新增 `write_batch_rejects_for_edge_group_when_target_not_found`，P0 第 2 项补齐 group-not-found 批量失败覆盖，当前剩余“更细错误语义边界”收敛。
- `2026-03-11 23:16`：完成“系统表缺失 vs 目标不存在”语义拆分，新增错误码 `TARGET_METADATA_MISSING`，并补齐 `write`/`write_batch` 缺系统表回归测试（device/group）；P0 第 3 项由“待实现”推进为“进行中（已完成核心分流）”。
- `2026-03-12 00:08`：新增 `write_rejects_for_edge_group_when_metadata_table_missing`，补齐 `write/group` 缺系统表对称覆盖；P0 第 2/3 项从“进行中”推进为“完成（leader 侧单条+批量覆盖齐全）”。
- `2026-03-12 00:08`：新增 follower 转发回归 `router_forward_preserves_target_metadata_missing_for_edge_write` 与 `router_forward_preserves_target_metadata_missing_for_edge_write_batch`，并修正 follower 写路径为“转发原始 SQL（保留路由尾缀）”；确认 `TARGET_METADATA_MISSING` 可在 leader→follower 链路稳定透传。
- `2026-03-12 00:22`：P1 从“冻结语义”推进到“可执行预研拆解”，已固定 AddEdge 双场景首批失败用例命名与核心断言边界；保持“仅计划不编码”的迭代约束不变。
- `2026-03-12 00:34`：继续向下推进 P1 开工入口：补齐 AddEdge 立项后的“测试文件入口 / 实现文件入口 / 最小回归命令”映射，确保从计划可直接跳转到 TDD 开工点。
- `2026-03-12 00:47`：继续向下推进 P1 门禁定义：新增“立项触发条件 / 执行顺序 / 验收门禁”三段式约束，目的为防止 AddEdge 立项时出现“先写实现后补测试”与“错误语义混码”回归。
- `2026-03-12 00:58`：继续向下推进 P1 可执行性：新增“双场景首批失败测试断言模板”与“最小/扩展回归矩阵门禁”，确保 AddEdge 立项时可直接按断言与矩阵进入 TDD。
- `2026-03-12 01:35`：按冻结断言将 AddEdge 首批四条测试落地到 `tests/cluster_admin_add_hub.rs`，并在 `src/management/cluster_admin_service.rs` 增加不改 proto/CLI 的最小内部实现（可注入 `EdgeMetadataProvider`）；之所以改这里而不是 proto/CLI，是为了先验证语义分流与错误码边界，再决定对外接口形态。
- `2026-03-12 01:35`：回归门禁执行结果：`cargo test --test cluster_admin_add_hub -- --nocapture`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo check --all-targets --all-features` 通过；并对 `remove_hub_commit_auto_transfers_leader_and_succeeds` 的偶发 leader 转移超时增加测试内重试以降低并发抖动误报。
- `2026-03-12 02:12`：新增 follower 转发回归 `addedge_on_follower_forwards_and_preserves_target_metadata_missing` 与 `addedge_on_follower_forwards_and_preserves_target_not_found`，并在 `cluster_admin_service` 增加可注入 `AddEdgeForwardTarget` 的最小转发机制，确保 Matrix-5/Matrix-6 错误码透传稳定；之所以继续改管理层而不是 proto/CLI，是为了先收敛内部转发语义，再决定外部接口暴露节奏。
