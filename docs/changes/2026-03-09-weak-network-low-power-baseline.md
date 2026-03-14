# 弱网与低能耗场景参数基线方案

**日期**: 2026-03-09  
**作者**: Trae AI (Pair Programmer)  
**状态**: 草案（可直接用于第一轮落地）

## 1. 背景与目标
当前系统在常规网络与资源条件下可运行，但在弱网抖动、设备休眠、低功耗硬件约束下，现有默认参数更偏向吞吐，容易在稳定性与功耗上出现放大效应。  
本方案目标如下：

- 给出可直接落地的三档参数基线（开发/工控/电池）。
- 保持现有协议与代码结构不变，优先通过配置收敛风险。
- 同时提供最小回归用例清单，确保参数调整后可验证、可回滚。

## 2. 适用范围
本方案围绕以下现有配置与运行逻辑：

- 网关配置结构：`src/config.rs` 中 `EdgeGatewayConfig`
- 网关并发与批处理逻辑：`src/hub/edge_gateway.rs`
- 现有压测与重试行为验证：`tests/edge_tcp_stress.rs`

## 3. 方案选型
### 3.1 方案 A：三档静态基线 + 回归门禁（推荐）
- 思路：一次性定义三套可切换参数模板，按环境选择，配套最小回归检查。
- 优点：实现最快、风险最可控、便于灰度。
- 缺点：无法自动适配实时网络波动。

### 3.2 方案 B：三档基线 + 轻量动态调参
- 思路：在 A 的基础上按运行指标（超时率/队列深度）自动升降档。
- 优点：长期收益更高，适应波动更强。
- 缺点：实现复杂、调试成本较高，短期不建议先做。

### 3.3 方案 C：按设备型号固定分支配置
- 思路：每种设备型号单独维护一套参数。
- 优点：单设备表现可做到最优。
- 缺点：维护与发布复杂，配置漂移风险高。

## 4. 推荐基线参数（三档）
### 4.1 开发档（Dev）
用于本地调试、功能验证、有限并发测试：

```yaml
max_connections: 200
batch_max_size: 128
batch_max_delay_ms: 8
batch_max_queue_size: 10000
batch_wait_timeout_ms: 120
batch_shards: 2
ordering_stage_parallelism: 2
ordering_queue_limit: 5000
pre_agg_enabled: false
pre_agg_queue_size: 5000
```

### 4.2 工控档（Industrial）
用于稳定供电、长期在线、吞吐与稳定平衡场景：

```yaml
max_connections: 500
batch_max_size: 192
batch_max_delay_ms: 12
batch_max_queue_size: 20000
batch_wait_timeout_ms: 220
batch_shards: 4
ordering_stage_parallelism: 4
ordering_queue_limit: 20000
pre_agg_enabled: true
pre_agg_queue_size: 20000
```

### 4.3 电池档（Battery）
用于低功耗设备、上报稀疏、网络波动较明显场景：

```yaml
max_connections: 80
batch_max_size: 64
batch_max_delay_ms: 25
batch_max_queue_size: 3000
batch_wait_timeout_ms: 350
batch_shards: 1
ordering_stage_parallelism: 1
ordering_queue_limit: 1000
pre_agg_enabled: true
pre_agg_queue_size: 2000
```

## 5. 参数含义与取舍说明
### 5.1 稳定性优先参数
- `batch_wait_timeout_ms`：弱网下应适度提高，避免抖动导致大量误超时。
- `ordering_queue_limit`：限制顺序域排队深度，防止局部热点拖垮整体。
- `max_connections`：不要盲目拉高，连接过多会加剧调度争用与内存压力。

### 5.2 低功耗优先参数
- `batch_max_delay_ms`：电池档适当提高，减少频繁小包提交与 CPU 唤醒次数。
- `batch_shards` 与 `ordering_stage_parallelism`：电池档建议最低化，降低常驻并发开销。
- `batch_max_queue_size`/`pre_agg_queue_size`：控制上限，避免内存峰值与系统抖动。

### 5.3 吞吐优先参数
- `batch_max_size`：工控档可提高，以更高聚合换吞吐。
- `pre_agg_enabled`：在稳定供电与中高并发场景开启收益更明显。

## 6. 落地步骤（建议顺序）
1. 先在开发环境落地“开发档”，验证功能回归与基础稳定性。
2. 在预发/工控测试环境落地“工控档”，观察 24h 指标。
3. 在电池设备灰度落地“电池档”，观察功耗与超时率。
4. 通过统一看板对比三档指标后，再决定是否引入动态调参（方案 B）。

## 7. 最小回归用例清单
以下用例用于参数变更后的最小可信验证：

1. 弱网抖动（高 RTT + 抖动）下，验证成功率与平均等待时延。
2. 短时断连重连，验证连接许可释放与恢复能力。
3. 队列接近上限时，验证无雪崩、无长时间阻塞。
4. 顺序规则命中场景，验证顺序等待超时不异常放大。
5. 预聚合开启场景，验证业务失败路径能进入重试并最终收敛。
6. 电池档长稳态运行，验证超时率、CPU 占用与功耗指标。
7. 重启恢复场景，验证 A/B 槽与 ACK 行为符合预期。

## 8. 风险与不可完全消除项
### 8.1 可优化风险
- 参数默认值偏吞吐，易在低资源设备上放大功耗与抖动。
- 队列与并发上限过大时，极端情况下会出现内存压力与排队放大。

### 8.2 架构级硬约束
- 强一致系统在网络分区下写可用性下降属于协议属性，无法通过单纯调参消除。
- 断电安全与低功耗存在天然冲突：更高持久化频率通常意味着更高功耗。
- 快速故障切换与避免误选举存在取舍，无法同时最优。

## 9. 下一步建议
- 在 `docs/changes` 后续补一篇“基线实测报告”，统一记录三档指标对比（成功率、P95、超时率、CPU、内存、功耗）。
- 若工控档连续稳定，再评估是否进入“轻量动态调参”阶段。
