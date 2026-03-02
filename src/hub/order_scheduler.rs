use anyhow::{Result, anyhow};
use std::collections::{BTreeMap, HashMap, VecDeque};
use tokio::sync::Mutex;

// ### 修改记录 (2026-03-01)
// - 原因: 需要对跨设备顺序域进行阶段化调度
// - 目的: 保证同一顺序域内阶段屏障生效
// - 备注: 调度器内部使用 Mutex 保障简单并发安全
pub struct OrderingScheduler<T> {
    stage_parallelism: usize,
    queue_limit: usize,
    state: Mutex<SchedulerState<T>>,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要向上游返回已就绪的任务
// - 目的: 让网关仅处理当前阶段允许执行的任务
// - 备注: task 由调用方定义，可承载 SQL 或测试载荷
#[derive(Debug)]
pub struct ReadyTask<T> {
    pub group: String,
    pub stage: u32,
    pub task: T,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要维护每个顺序域的阶段队列
// - 目的: 在阶段内允许并行，阶段间强制屏障
// - 备注: stages 使用 BTreeMap 便于按阶段升序推进
struct GroupQueue<T> {
    current_stage: u32,
    inflight: usize,
    stages: BTreeMap<u32, VecDeque<T>>,
    in_ready_queue: bool,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要集中维护调度器的全局状态
// - 目的: 控制总队列上限并支持多组并行
// - 备注: queue_len 仅统计尚未调度的任务
struct SchedulerState<T> {
    groups: HashMap<String, GroupQueue<T>>,
    ready_groups: VecDeque<String>,
    queue_len: usize,
}

impl<T> OrderingScheduler<T> {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要创建调度器并设置并行度与队列上限
    // - 目的: 控制每组阶段并行度并限制内存占用
    // - 备注: stage_parallelism 至少为 1
    pub fn new(stage_parallelism: usize, queue_limit: usize) -> Self {
        Self {
            stage_parallelism: stage_parallelism.max(1),
            queue_limit,
            state: Mutex::new(SchedulerState {
                groups: HashMap::new(),
                ready_groups: VecDeque::new(),
                queue_len: 0,
            }),
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要将任务放入对应顺序域与阶段队列
    // - 目的: 为阶段屏障提供输入队列
    // - 备注: 总队列超限时拒绝入队
    pub async fn enqueue(&self, group: &str, stage: u32, task: T) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.queue_len >= self.queue_limit {
            return Err(anyhow!("Ordering queue full"));
        }
        state.queue_len = state.queue_len.saturating_add(1);

        let should_schedule = {
            let queue = state
                .groups
                .entry(group.to_string())
                .or_insert_with(|| GroupQueue {
                    current_stage: stage,
                    inflight: 0,
                    stages: BTreeMap::new(),
                    in_ready_queue: false,
                });
            let stage_queue = queue.stages.entry(stage).or_insert_with(VecDeque::new);
            stage_queue.push_back(task);

            if !queue.in_ready_queue && can_schedule(queue, self.stage_parallelism) {
                queue.in_ready_queue = true;
                true
            } else {
                false
            }
        };

        if should_schedule {
            state.ready_groups.push_back(group.to_string());
        }
        Ok(())
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要高效获取可执行的任务 (O(1))
    // - 目的: 避免线性扫描导致的性能瓶颈
    // - 备注: 使用 ready_groups 队列轮询就绪组
    pub async fn next_ready(&self) -> Option<ReadyTask<T>> {
        let mut state = self.state.lock().await;
        
        while let Some(group_id) = state.ready_groups.pop_front() {
            let (maybe_task, should_requeue) = {
                let queue = match state.groups.get_mut(&group_id) {
                    Some(q) => q,
                    None => continue,
                };
                
                queue.in_ready_queue = false;

                advance_group_if_needed(queue);

                if !can_schedule(queue, self.stage_parallelism) {
                    (None, false)
                } else {
                    let stage = queue.current_stage;
                    if let Some(stage_queue) = queue.stages.get_mut(&stage)
                        && let Some(task) = stage_queue.pop_front()
                    {
                        queue.inflight = queue.inflight.saturating_add(1);
                        if stage_queue.is_empty() {
                            queue.stages.remove(&stage);
                        }
                        
                        let requeue = if can_schedule(queue, self.stage_parallelism) {
                            queue.in_ready_queue = true;
                            true
                        } else {
                            false
                        };

                        (Some(ReadyTask {
                            group: group_id.clone(),
                            stage,
                            task,
                        }), requeue)
                    } else {
                        (None, false)
                    }
                }
            };

            if should_requeue {
                state.ready_groups.push_back(group_id);
            }

            if let Some(task) = maybe_task {
                state.queue_len = state.queue_len.saturating_sub(1);
                return Some(task);
            }
        }
        None
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在任务完成后推进阶段
    // - 目的: 当阶段内任务清空时解除下一阶段屏障
    // - 备注: 当前实现通过 inflight 计数确保阶段内收敛
    pub async fn mark_done(&self, group: &str, stage: u32) {
        let mut state = self.state.lock().await;
        
        let should_schedule = {
            let queue = match state.groups.get_mut(group) {
                Some(q) => q,
                None => return,
            };
            if queue.current_stage != stage {
                return;
            }
            queue.inflight = queue.inflight.saturating_sub(1);
            advance_group_if_needed(queue);

            if !queue.in_ready_queue && can_schedule(queue, self.stage_parallelism) {
                queue.in_ready_queue = true;
                true
            } else {
                false
            }
        };

        if should_schedule {
            state.ready_groups.push_back(group.to_string());
        }
    }
}

fn can_schedule<T>(queue: &GroupQueue<T>, parallelism: usize) -> bool {
    if queue.inflight >= parallelism {
        return false;
    }
    queue.stages.contains_key(&queue.current_stage)
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在阶段清空后寻找下一阶段
// - 目的: 保证阶段屏障在阶段内全部完成后推进
// - 备注: 仅在当前阶段队列为空且无在途任务时推进
fn advance_group_if_needed<T>(queue: &mut GroupQueue<T>) {
    if queue.inflight > 0 {
        return;
    }
    // ### 修改记录 (2026-03-01)
    // - 原因: clippy 提示可合并条件判断
    // - 目的: 保持阶段推进逻辑一致
    if queue
        .stages
        .get(&queue.current_stage)
        .map(|q| q.is_empty())
        .unwrap_or(true)
        && let Some((&next_stage, _)) = queue.stages.iter().next()
    {
        queue.current_stage = next_stage;
    }
}
