# 统一 SQL 语法规范（对齐标准 SQL / Oracle 风格）- Hub & Edge

## 修改记录 (2026-03-10)
- **原因**：需要一份专门文档，明确“客户端只学习一套 SQL”的完整规范，并对分组/设备维护策略给出可执行方案。
- **目的**：统一 DDL/DML 入口、固定一致性边界、明确元数据维护责任，避免出现第二套 DSL。
- **时间**：2026-03-10

## 1. 文档目标与范围
- 本规范只定义一套 SQL 入口，语义尽量与标准 SQL / Oracle 风格一致。
- 本规范仅增加路由后缀，不改变 SQL 主体语法。
- 不引入触发器、存储过程、函数扩展，不做额外方言增强。
- SQLite 不支持的能力，本规范不做“兼容魔改”。

## 2. 设计原则
- 客户侧仅使用 SQL，不区分 Hub/Edge 底层实现。
- DDL 采用强一致硬约束（强卡），防止元数据分裂。
- DML 默认强一致，可在字段级按规范声明最终一致。
- 路由信息与业务 SQL 分离，便于解析、审计、回放与故障排查。

## 3. 统一语法

### 3.1 路由后缀
- `FOR HUB`：在 Hub 执行。
- `FOR EDGE(device_id=<id>)`：在单设备 Edge 执行。
- `FOR EDGE(device_id=all)`：在全量设备或目标编组执行。
- `FOR EDGE(group_id=<gid>)`：在指定分组执行。

### 3.2 一致性规则
- 默认一致性：`strong`。
- 字段级覆盖：列定义可写 `/* consistency:eventual */`。
- DDL 强卡：所有 DDL（CREATE/DROP/ALTER）强制 `strong`，不允许 eventual。

### 3.3 DDL 示例
```sql
CREATE TABLE orders (
  id VARCHAR(64) PRIMARY KEY,
  amount DECIMAL(18,2) NOT NULL
) FOR HUB;

CREATE TABLE telemetry (
  id VARCHAR(64) PRIMARY KEY /* consistency:strong */,
  temp DOUBLE /* consistency:eventual */,
  humidity DOUBLE /* consistency:eventual */
) FOR EDGE(group_id=12);

ALTER TABLE telemetry ADD COLUMN battery DOUBLE /* consistency:eventual */ FOR EDGE(group_id=12);

DROP TABLE telemetry FOR EDGE(group_id=12);
```

### 3.4 DML 示例
```sql
INSERT INTO orders(id, amount) VALUES ('A-001', 100.00) FOR HUB;

INSERT INTO telemetry(id, temp, humidity)
VALUES ('dev-3201', 28.6, 61.2)
FOR EDGE(device_id=3201);

UPDATE telemetry
SET temp = 29.1
WHERE id = 'dev-3201'
FOR EDGE(device_id=3201);

SELECT id, amount FROM orders WHERE id = 'A-001' FOR HUB;
```

## 4. 与标准 SQL / Oracle 风格的对应关系
- 保留 `CREATE TABLE / DROP TABLE / ALTER TABLE / INSERT / UPDATE / DELETE / SELECT` 主体结构。
- 仅新增尾部路由子句 `FOR ...`，不改列定义、约束、谓词表达式。
- 不要求用户学习 `.sql 文件编排 DSL` 才能执行单条语句。
- 若未来提供 CLI，CLI 仅做传输与批量执行，不发明第二语法。

## 5. 强卡规则（DDL 强一致硬约束）
- 规则 1：DDL 只能走强一致提交路径。
- 规则 2：DDL 不允许字段级 eventual 覆盖到主键、唯一键、索引键。
- 规则 3：同一事务内含 DDL + DML 时，以 DDL 强一致规则为上限。
- 规则 4：DDL 在目标路由不可达时失败返回，不做“静默部分成功”。

## 6. 分组关系与设备关系维护方案

### 6.1 方案 A：系统表维护（推荐）
- 在 Hub 维护元数据系统表，作为唯一事实源。
- 建议系统表：
  - `sys_edge_device(device_id, group_id, status, tags, updated_at)`
  - `sys_edge_group(group_id, group_name, policy, updated_at)`
  - `sys_table_route(schema_name, table_name, target_type, target_value, ddl_consistency, updated_at)`
  - `sys_column_consistency(schema_name, table_name, column_name, consistency_level, updated_at)`
- 建议表结构定义（SQLite 方言，按强一致 DDL 管理）：

```sql
CREATE TABLE IF NOT EXISTS sys_edge_group (
  group_id TEXT PRIMARY KEY,
  group_name TEXT NOT NULL,
  policy TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'active',
  version INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK (status IN ('active', 'disabled'))
);

CREATE TABLE IF NOT EXISTS sys_edge_device (
  device_id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'online',
  tags TEXT NOT NULL DEFAULT '[]',
  endpoint TEXT,
  heartbeat_at TEXT,
  version INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK (status IN ('online', 'offline', 'disabled')),
  FOREIGN KEY (group_id) REFERENCES sys_edge_group(group_id)
);
CREATE INDEX IF NOT EXISTS idx_sys_edge_device_group_id ON sys_edge_device(group_id);
CREATE INDEX IF NOT EXISTS idx_sys_edge_device_status ON sys_edge_device(status);

CREATE TABLE IF NOT EXISTS sys_table_route (
  schema_name TEXT NOT NULL DEFAULT 'main',
  table_name TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_value TEXT NOT NULL,
  ddl_consistency TEXT NOT NULL DEFAULT 'strong',
  dml_default_consistency TEXT NOT NULL DEFAULT 'strong',
  version INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (schema_name, table_name),
  CHECK (target_type IN ('hub', 'edge_device', 'edge_group', 'edge_all')),
  CHECK (ddl_consistency IN ('strong')),
  CHECK (dml_default_consistency IN ('strong', 'eventual'))
);
CREATE INDEX IF NOT EXISTS idx_sys_table_route_target ON sys_table_route(target_type, target_value);

CREATE TABLE IF NOT EXISTS sys_column_consistency (
  schema_name TEXT NOT NULL DEFAULT 'main',
  table_name TEXT NOT NULL,
  column_name TEXT NOT NULL,
  consistency_level TEXT NOT NULL DEFAULT 'strong',
  is_key_column INTEGER NOT NULL DEFAULT 0,
  version INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (schema_name, table_name, column_name),
  CHECK (consistency_level IN ('strong', 'eventual')),
  CHECK (is_key_column IN (0, 1)),
  FOREIGN KEY (schema_name, table_name) REFERENCES sys_table_route(schema_name, table_name)
);
CREATE INDEX IF NOT EXISTS idx_sys_column_consistency_level
ON sys_column_consistency(schema_name, table_name, consistency_level);
```
- 写入约束建议：
  - `sys_table_route.ddl_consistency` 固定为 `strong`，不允许变更为 `eventual`。
  - `sys_column_consistency.is_key_column=1` 的列，`consistency_level` 必须为 `strong`。
  - 所有元数据变更必须带 `version = old_version + 1` 的乐观并发检查，避免并发覆盖。
- 运行缓存建议：
  - Hub 维护内存路由缓存，按 `(schema_name, table_name, version)` 缓存。
  - 当系统表版本递增时强制失效缓存并重载，保证路由判定单调更新。
- 优点：
  - 与 SQL 生态一致，可审计、可查询、可事务化。
  - 变更可纳入 Raft 强一致链路，避免多处配置漂移。
  - 便于后续做权限、血缘、灰度策略。
- 缺点：
  - 需要先建设元数据表与管理接口。
  - 初期实现量高于纯配置文件。

### 6.2 方案 B：配置文件维护
- 使用 YAML/TOML/JSON 保存分组与设备映射。
- 优点：
  - 上手快，实现成本低。
  - 适合 PoC 或单集群短周期验证。
- 缺点：
  - 配置易漂移，审计与回溯弱。
  - 多节点同步与热更新复杂，易出现版本不一致。
  - 与 SQL 生命周期割裂，运维复杂度高。

### 6.3 方案 C：混合维护（系统表为主，配置文件引导）
- 启动时允许配置文件导入系统表；运行期以系统表为准。
- 优点：
  - 兼顾冷启动效率与长期治理。
  - 可平滑从现有配置迁移到系统表。
- 缺点：
  - 需要定义导入优先级、冲突策略、幂等规则。

### 6.4 推荐结论
- 推荐 **方案 A（系统表维护）** 作为最终形态。
- 可短期采用 **方案 C** 作为迁移路径：配置文件只负责 bootstrap，运行后全部收敛到系统表。

## 7. 错误码建议（首版）
- `ROUTE_INVALID`：`FOR` 子句非法或缺失必要参数。
- `TARGET_NOT_FOUND`：`device_id/group_id` 不存在。
- `DDL_STRONG_REQUIRED`：DDL 请求了非强一致策略。
- `COLUMN_CONSISTENCY_INVALID`：字段一致性声明违反约束（例如主键标为 eventual）。
- `SQLITE_UNSUPPORTED`：语句超出当前 SQLite 支持边界。
- `PARTIAL_ROUTE_FORBIDDEN`：目标编组部分不可达，不允许部分成功。

## 8. 明确不做
- 不做 `CREATE TRIGGER`。
- 不做存储过程、函数扩展。
- 不做脱离 SQL 的第二套业务 DSL。
- 不做 SQLite 未支持语义的兼容性改造。

## 9. 落地顺序建议
- 第一步：落地系统表模型与读写 API。
- 第二步：实现 SQL 尾部 `FOR` 解析与路由绑定。
- 第三步：实现 DDL 强卡与字段一致性校验。
- 第四步：补齐错误码、审计、回归测试矩阵。

## 10. 实施计划（可执行版）
- Phase 1：元数据底座
  - 产出：4 张系统表建表脚本、初始化迁移脚本、基础查询接口。
  - 验收：可完成 group/device/route/column 的增删改查，且 `version` 单调递增。
- Phase 2：SQL 路由解析
  - 产出：`FOR HUB`、`FOR EDGE(device_id=...)`、`FOR EDGE(group_id=...)`、`FOR EDGE(device_id=all)` 解析器。
  - 验收：对合法/非法语句给出稳定错误码（`ROUTE_INVALID`、`TARGET_NOT_FOUND`）。
- Phase 3：强卡与一致性校验
  - 产出：DDL 强一致拦截、字段级 eventual 校验、键列强一致校验。
  - 验收：DDL 非 strong 一律失败；键列 eventual 一律失败（`COLUMN_CONSISTENCY_INVALID`）。
- Phase 4：配置文件导入与收敛
  - 产出：配置文件一次性导入系统表能力（可选），导入幂等校验。
  - 验收：重复导入不产生重复绑定，冲突按版本与策略可预测处理。
- Phase 5：回归与上线门禁
  - 产出：DDL/DML 路由回归用例、错误码回归用例、异常场景用例。
  - 验收：`cargo test` 全通过，文档与实现一致，管理面审计可追溯。
