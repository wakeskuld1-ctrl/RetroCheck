# 稳定性与健壮性巡检报告

**日期**: 2026-03-05  
**作者**: Trae AI (Pair Programmer)  
**状态**: 已完成

## 1. 巡检范围
- `src/hub/edge_gateway.rs`
- `src/config.rs`
- `src/hub/edge_session_schema.rs`
- `src/raft/raft_node.rs`
- `src/wal.rs`
- `src/raft/network.rs`
- `tests/edge_tcp_stress.rs`

## 2. 总体结论
当前代码已经具备并发限制、批处理超时、黑名单、重试等稳定性机制，但仍存在若干会在极端流量或异常路径下放大故障的风险点。主要集中在：
- 连接生命周期控制不足导致并发槽位被长期占用
- 配置加载缺少边界清洗导致“可启动但不可服务”
- 生产路径中存在 `panic/unwrap/expect` 的故障放大点
- 压测故障注入窗口与短时场景不匹配，影响可重复性

## 修复进展（截至 2026-03-05）
- 3.1 连接可长期占用并发槽位：已修复
- 3.2 EdgeGateway 配置缺少 sanitize：已修复
- 3.3 生产路径存在 panic/expect：已修复
- 3.4 Mutex poison 后可能连锁失败：已修复
- 3.5 压测故障注入窗口过长：未修复

## 3. 详细问题清单

### 3.1 高优先级：连接可长期占用并发槽位（状态：已修复）
**现象**
- 连接处理循环基于 `framed.next().await` 持续等待输入
- 未发现连接级读超时/空闲超时控制
- 并发许可在连接任务结束前不会归还

**影响**
- 慢连接或空闲连接可长期占住 `Semaphore` 许可
- 在高并发下会出现健康请求被阻塞、吞吐骤降

**证据位置**
- `src/hub/edge_gateway.rs:2005-2017`
- `src/hub/edge_gateway.rs:2181-2364`

**修复位置**
- `src/hub/edge_gateway.rs:2181-2192`
- `tests/edge_tcp_limits.rs:171-191`

---

### 3.2 高优先级：EdgeGateway 配置缺少 sanitize（状态：已修复）
**现象**
- `EdgeGatewayConfig::load_from_file` 直接反序列化返回
- 未做类似 `EdgeConfig::sanitize` 的边界修正

**影响**
- 例如 `max_connections=0` 时，服务可启动但永远无法获取连接许可
- 配置错误会在运行期表现为“假活性”，排查成本高

**证据位置**
- `src/config.rs:293-299`
- `src/hub/edge_gateway.rs:1764`

**修复位置**
- `src/config.rs:294-371`
- `tests/edge_tcp_config_security.rs:72-94`

---

### 3.3 中优先级：生产路径存在 panic/expect（状态：已修复）
**现象**
- 会话编解码路径含 `panic!` 与多处 `expect`
- Raft 启动配置校验使用 `unwrap`

**影响**
- 输入异常或内部状态异常时，错误可能直接升级为进程级崩溃
- 降低系统在异常流量下的故障隔离能力

**证据位置**
- `src/hub/edge_session_schema.rs:98-114`
- `src/hub/edge_session_schema.rs:299-303`
- `src/raft/raft_node.rs:103-110`

**修复位置**
- `src/hub/edge_session_schema.rs:98-332`
- `src/hub/edge_gateway.rs:757-783`
- `tests/edge_schema_test.rs:50-74`
- `src/raft/raft_node.rs:60-65`
- `src/raft/raft_node.rs:109-113`
- `src/raft/raft_node.rs:480-499`

---

### 3.4 中优先级：Mutex poison 后可能连锁失败（状态：已修复）
**现象**
- 多个核心路径采用 `lock().unwrap()`

**影响**
- 一旦前序任务 panic 造成 poisoned mutex，后续请求可能连续失败
- 容错行为由“降级报错”退化为“二次崩溃”

**证据位置**
- `src/hub/edge_gateway.rs:2211-2212`
- `src/wal.rs:30-45`
- `src/raft/network.rs:59-74`

**修复位置**
- `src/wal.rs:30-67`
- `src/wal.rs:70-105`
- `src/hub/edge_gateway.rs:1879-1905`
- `src/hub/edge_gateway.rs:2172-2334`
- `src/raft/network.rs:25-30`
- `src/raft/network.rs:66-135`
- `src/raft/network.rs:238-259`
- `src/raft/network.rs:274-323`

---

### 3.5 中优先级：压测故障注入窗口过长，短测波动大（状态：未修复）
**现象**
- 中途故障注入停机时长固定在 10_000~30_000ms
- 对 2~5 秒短压测，注入窗口远大于测试主体

**影响**
- 用例总耗时和结果抖动显著增大
- CI/回归基线稳定性下降

**证据位置**
- `tests/edge_tcp_stress.rs:2190-2208`
- `tests/edge_tcp_stress.rs:2286-2290`

## 4. 建议修复优先级
1. 连接空闲超时与读超时机制（最高优先级）
2. `EdgeGatewayConfig` 增加 sanitize 与非法值回退
3. 生产路径移除 `panic/unwrap/expect`，改为显式错误返回
4. 故障注入停机窗口改为按 `case.duration` 比例并加上限
5. 核心锁路径增加 poisoned 处理策略（降级、重建或错误透传）

## 5. 建议补充测试用例
- 配置边界：`max_connections=0`、`batch_shards=0`、`batch_max_size=0`
- 慢连接与空连接：验证超时释放许可
- poisoned mutex 场景：验证后续请求返回可观测错误而非崩溃
- 短时压测（2s/5s）下 failover 注入：验证总耗时上限与结果稳定性

## 6. 备注
- 本文档仅为巡检报告，不包含代码修复提交。
- 若进入修复阶段，建议先从连接超时与配置 sanitize 两项着手，可快速提升系统可用性。
