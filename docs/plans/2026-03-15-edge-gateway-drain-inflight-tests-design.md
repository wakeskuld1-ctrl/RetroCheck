# EdgeGateway 排空连接拒绝与 inflight 统计回归测试设计

日期: 2026-03-15

## 背景
- 需要覆盖“排空期间拒绝新连接，客户端连接重试”的回归场景。
- 需要验证 inflight_requests 在并发连接时统计一致性。
- 需要清理 edge_generated.rs 的 Rust 2024 警告，避免生成代码引入噪音。
- 现有部分备注存在 ANSI 乱码，需要恢复为 UTF-8 便于维护。

## 目标
- 排空期间 `TcpStream::connect` 失败（A 判定），恢复后可重新连接。
- 并发连接场景下 inflight_requests 统计与连接生命周期一致。
- 编译时不再出现 edge_generated.rs 的 Rust 2024 警告。
- 修复相关文件的 ANSI 乱码备注为 UTF-8。

## 非目标
- 不改变业务协议与生产配置默认值。
- 不引入新的功能特性，仅补测试与必要的连接行为调整。

## 方案与选择
### 方案A（已确认）
- 排空期间关闭监听 socket，解除排空时重新 bind 同端口。
- 优点: connect 直接失败，满足 A 判定。
- 缺点: Windows 可能需要重试/短暂 backoff。

### 方案B（不选）
- 保持监听，accept 后立即关闭。
- 缺点: connect 仍会成功，无法满足 A 判定。

### 方案C（不选）
- 迁移监听到黑洞端口。
- 缺点: 语义变化大，复杂度高。

## 行为与架构设计
1. 排空时：关闭当前 TcpListener，避免 accept。
2. 解除排空：重新 bind 到原地址端口。
3. 测试侧需要获取 EdgeGateway 句柄以调用 set_draining。
4. inflight_requests 测试通过并发连接验证计数与生命周期一致性。
5. edge_generated.rs 警告通过 include 包裹 allow 或生成参数控制消除。

## 测试设计
- 排空期间 connect 失败回归测试：draining=true 时多次连接应失败；draining=false 后连接成功。
- inflight_requests 并发一致性：并发 N 连接 -> 计数 >= N；连接释放后归零。
- 编译验证：cargo test -- --test-threads=1，无 edge_generated.rs 警告。

## 风险与缓解
- 监听重绑的短暂失败：测试中加入小步重试，生产逻辑可考虑 backoff。
- inflight 计数时序波动：测试使用同步手段（Barrier/通知）降低抖动。

## 验收标准
- 新增测试通过且稳定。
- connect 在 draining 期间稳定失败。
- edge_generated.rs 警告消失。
- 相关文件备注全部为 UTF-8。
