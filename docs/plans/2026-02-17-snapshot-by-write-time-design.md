# Write-Time Snapshot Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans after this design is approved and committed.

**Goal**  
基于“数据库写入时间”触发快照切割，解决日志无限增长与恢复过慢问题，保持对外协议不变，并为后续 Raft 日志压缩预留接口。

**Scope**  
- 快照触发条件：以写入时间为主触发，日志条数为兜底触发  
- 快照生成方式：SQLite `VACUUM INTO`（非阻塞一致性备份）  
- 快照元数据：记录上次快照时间与日志索引  
- 崩溃恢复：可基于元数据判断快照边界  

**Non-Goals**  
- 不实现完整的 OpenRaft Snapshot API 对接  
- 不实现读扩展（ReadIndex）  
- 不实现批处理或动态成员变更  

---

## Options

### Option A: 写入后记录数据库时间（推荐）
- **方式**：每次写入成功后，在同一 SQLite 内更新 `_raft_meta.last_write_at`  
- **优点**：时间语义正确、崩溃一致性高、可重放恢复  
- **缺点**：每次写入多一次元数据更新  

### Option B: 写入后记录到 RaftStore(sled)
- **方式**：写入成功后从 SQLite 取时间并写入 sled  
- **优点**：不改表结构  
- **缺点**：DB 与 sled 时间可能不一致，崩溃边界不稳  

### Option C: 依赖 WAL 中系统时间
- **方式**：WAL 中记录系统时间并据此切割  
- **优点**：实现简单  
- **缺点**：系统时间漂移、回拨导致语义不可靠  

**Decision**  
采用 **Option A**。  

---

## Data Model

**SQLite 系统表** `_raft_meta`（如果不存在则创建）

| 字段 | 类型 | 说明 |
|---|---|---|
| key | TEXT PRIMARY KEY | 元数据键 |
| value | TEXT | 元数据值 |

**元数据键约定**
- `last_write_at`：最近一次成功写入的数据库时间戳（秒或毫秒）  
- `last_snapshot_at`：最近一次快照的写入时间戳  
- `last_snapshot_index`：最近一次快照对应的日志索引  

---

## Snapshot Trigger

**主触发：写入时间窗口**  
```
if last_write_at - last_snapshot_at >= 24h -> 触发快照
```

**兜底触发：日志条数**  
```
if last_applied - last_snapshot_index >= N -> 触发快照
```

说明：时间窗口用于满足“按写入时间切割”，日志条数用于防止长时间无快照导致恢复过慢。

---

## Snapshot Flow

1. **写入成功**：在同一 SQLite 内更新 `_raft_meta.last_write_at`  
2. **后台检测**：定期检查触发条件  
3. **生成快照**：执行 `VACUUM INTO 'snapshot_<start>_<end>.db'`  
4. **更新元数据**：写入 `_raft_meta.last_snapshot_at` 与 `_raft_meta.last_snapshot_index`  
5. **日志截断**：通知 Raft 层截断旧日志（后续接入 OpenRaft）  

---

## Failure Handling

- **VACUUM 失败**：不更新元数据，保留旧快照状态  
- **生成过程中崩溃**：下次启动检测残留快照文件，按元数据决定是否重试  
- **时间回拨**：以 SQLite 时间为准，若出现回拨则以 `max(last_snapshot_at, last_write_at)` 纠正  

---

## Operational Notes

- 快照文件命名：`snapshot_YYYYMMDDHHMMSS_YYYYMMDDHHMMSS.db`  
  - 起止时间与 `last_snapshot_at -> last_write_at` 对齐  
- 快照目录：与 DB 文件同目录，便于迁移  

---

## Tests

1. **时间触发**：模拟两次写入，覆盖 `>=24h` 触发  
2. **条数触发**：模拟日志增长到 N 条触发  
3. **崩溃恢复**：生成快照中断后重启，元数据保持一致  
4. **时间回拨**：模拟 `last_write_at < last_snapshot_at` 的修正逻辑  

