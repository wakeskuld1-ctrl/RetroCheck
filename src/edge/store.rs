//! ### 修改记录 (2026-02-25)
//! - 原因: 需要最小化 EdgeStore 定义以通过首个测试
//! - 目的: 提供可打开/关闭的 sled 句柄封装

// ### 修改记录 (2026-02-25)
// - 原因: 需要统一错误类型
// - 目的: 让上层测试使用 anyhow 处理错误
use anyhow::Result;

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要固定元数据键
/// - 目的: 保证活动指针读写一致
const ACTIVE_SLOT_KEY: &str = "edge_active_slot";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要统一日志前缀
/// - 目的: 支撑前缀扫描顺序
const LOG_PREFIX: &[u8] = b"log:";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要 TTL 空间前缀
/// - 目的: 区分 TTL 数据与其他键
const TTL_PREFIX: &[u8] = b"ttl:";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要记录物理状态序号
/// - 目的: 作为 HWM 推进前置条件
const STATE_SEQ_KEY: &str = "edge_state_seq";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要记录高水位线
/// - 目的: 支撑重放幂等判断
const HWM_KEY: &str = "edge_hwm";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要 cmd 命名空间前缀
/// - 目的: 前缀扫描覆盖指令数据
const CMD_PREFIX: &[u8] = b"cmd:";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要持久化 cmd 断点
/// - 目的: 断电恢复后续扫
const CMD_LAST_KEY: &str = "edge_cmd_last_key";

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要封装 sled::Db
/// - 目的: 统一 EdgeStore 对外接口
pub struct EdgeStore {
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化 KV
    /// - 目的: 支撑断电恢复与前缀扫描测试
    db: sled::Db,
}

impl EdgeStore {
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要从目录初始化存储
    /// - 目的: 允许断电重启复用同一目录
    pub fn open(path: &std::path::Path) -> Result<Self> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 打开可能失败
        // - 目的: 将错误交由上层处理
        Ok(Self { db: sled::open(path)? })
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化活动槽位
    /// - 目的: 支撑 A/B 切换断电恢复
    pub fn set_active_slot(&self, slot: u8) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 将 u8 转为单字节 Vec
        self.db.insert(ACTIVE_SLOT_KEY, vec![slot])?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要确保落盘
        // - 目的: 降低断电丢失风险
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取活动槽位
    /// - 目的: 重启后可确定活动实例
    pub fn get_active_slot(&self) -> Result<u8> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取持久化值
        // - 目的: 断电恢复时可取回
        let value = self.db.get(ACTIVE_SLOT_KEY)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 未设置时需默认值
        // - 目的: 保持调用方逻辑简单
        Ok(value.map(|v| v[0]).unwrap_or(0))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要构造可排序的日志键
    /// - 目的: 保障字典序等于时间序
    pub fn log_key(&self, ts: u64, seq: u64) -> Vec<u8> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要前缀区分日志空间
        // - 目的: 前缀扫描可只覆盖日志
        let mut key = Vec::with_capacity(LOG_PREFIX.len() + 16);
        key.extend_from_slice(LOG_PREFIX);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要保持大端排序
        // - 目的: 确保字典序与时间序一致
        key.extend_from_slice(&ts.to_be_bytes());
        key.extend_from_slice(&seq.to_be_bytes());
        key
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入日志记录
    /// - 目的: 为前缀扫描测试提供数据
    pub fn put_log(&self, ts: u64, seq: u64, value: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造日志键
        // - 目的: 保证排序语义
        let key = self.log_key(ts, seq);
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化日志内容
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可重放
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要前缀扫描日志键
    /// - 目的: 驱动待上传流水遍历
    pub fn scan_log_keys(&self, last_key: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要遍历前缀
        // - 目的: 限定扫描范围
        let mut result = Vec::new();
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要断点过滤
        // - 目的: 断电恢复从断点继续
        for item in self.db.scan_prefix(LOG_PREFIX) {
            let (key, _value) = item?;
            // ### 修改记录 (2026-02-25)
            // - 原因: 需要跳过已扫描键
            // - 目的: 避免重复上传
            if let Some(ref last) = last_key {
                if key.as_ref() <= last.as_slice() {
                    continue;
                }
            }
            result.push(key.to_vec());
        }
        Ok(result)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要构造 cmd 键
    /// - 目的: 支撑前缀扫描与断点续扫
    pub fn cmd_key(&self, ts: u64, seq: u64) -> Vec<u8> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要前缀区分 cmd 空间
        // - 目的: cmd 与 log 分离
        let mut key = Vec::with_capacity(CMD_PREFIX.len() + 16);
        key.extend_from_slice(CMD_PREFIX);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要保持大端排序
        // - 目的: 确保字典序与时间序一致
        key.extend_from_slice(&ts.to_be_bytes());
        key.extend_from_slice(&seq.to_be_bytes());
        key
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入 cmd 记录
    /// - 目的: 支撑断点重放与过期丢弃
    pub fn put_cmd(&self, ts: u64, seq: u64, expire_at_ms: u64, payload: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造 cmd 键
        // - 目的: 保证排序语义
        let key = self.cmd_key(ts, seq);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造二进制信封
        // - 目的: 前 8 字节存过期时间戳
        let mut value = Vec::with_capacity(8 + payload.len());
        value.extend_from_slice(&expire_at_ms.to_be_bytes());
        value.extend_from_slice(&payload);
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化 cmd 内容
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可重放
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入原始 cmd 数据
    /// - 目的: 为坏包测试提供注入入口
    pub fn put_cmd_raw(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 原样持久化测试数据
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后仍可读取
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要前缀扫描 cmd 键
    /// - 目的: 驱动断电重放指令
    pub fn scan_cmd_keys(&self, last_key: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要遍历前缀
        // - 目的: 限定扫描范围
        let mut result = Vec::new();
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要容错非法断点
        // - 目的: 非 cmd 前缀视为无断点
        let last_key = match last_key {
            Some(key) if key.starts_with(CMD_PREFIX) => Some(key),
            Some(_) => None,
            None => None,
        };
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要断点过滤
        // - 目的: 断电恢复从断点继续
        for item in self.db.scan_prefix(CMD_PREFIX) {
            let (key, _value) = item?;
            // ### 修改记录 (2026-02-25)
            // - 原因: 需要跳过已扫描键
            // - 目的: 避免重复执行
            if let Some(ref last) = last_key {
                if key.as_ref() <= last.as_slice() {
                    continue;
                }
            }
            result.push(key.to_vec());
        }
        Ok(result)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取 cmd payload
    /// - 目的: 支撑过期/坏包丢弃
    pub fn get_cmd_payload(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 可能不存在
        // - 目的: 简化调用方逻辑
        let raw = match self.db.get(key)? {
            Some(value) => value,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要校验长度
        // - 目的: 保护坏包
        if raw.len() < 8 {
            self.db.remove(key)?;
            self.db.flush()?;
            return Ok(None);
        }
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&raw[0..8]);
        let expire_at_ms = u64::from_be_bytes(ts_bytes);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要判断过期
        // - 目的: 过期直接丢弃
        if expire_at_ms <= Self::now_ms() {
            self.db.remove(key)?;
            self.db.flush()?;
            return Ok(None);
        }
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要返回 payload
        // - 目的: 跳过 TTL 头部
        Ok(Some(raw[8..].to_vec()))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化 cmd 断点
    /// - 目的: 断电重放从断点继续
    pub fn set_cmd_last_key(&self, key: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化断点
        self.db.insert(CMD_LAST_KEY, key)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后可恢复
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取 cmd 断点
    /// - 目的: 断电恢复可续扫
    pub fn get_cmd_last_key(&self) -> Result<Option<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 读取持久化值
        // - 目的: 断电恢复可取回
        Ok(self.db.get(CMD_LAST_KEY)?.map(|v| v.to_vec()))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要构造 TTL 键
    /// - 目的: 支撑 TTL 过期丢弃测试
    fn ttl_key(&self, key: u64) -> Vec<u8> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要前缀区分命名空间
        // - 目的: 避免与日志/元数据冲突
        let mut buf = Vec::with_capacity(TTL_PREFIX.len() + 8);
        buf.extend_from_slice(TTL_PREFIX);
        buf.extend_from_slice(&key.to_be_bytes());
        buf
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要封装当前时间
    /// - 目的: 统一 TTL 过期判断
    fn now_ms() -> u64 {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要系统时间戳
        // - 目的: 提供毫秒级过期判断
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        since_epoch.as_millis() as u64
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入带 TTL 的值
    /// - 目的: 允许过期直接丢弃
    pub fn put_with_ttl(&self, expire_at_ms: u64, payload: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要以过期时间作为 key
        // - 目的: 与测试调用保持一致
        let key = self.ttl_key(expire_at_ms);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造二进制信封
        // - 目的: 前 8 字节存过期时间戳
        let mut value = Vec::with_capacity(8 + payload.len());
        value.extend_from_slice(&expire_at_ms.to_be_bytes());
        value.extend_from_slice(&payload);
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化 TTL 值
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后仍可判断过期
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取带 TTL 的值
    /// - 目的: 过期时直接丢弃
    pub fn get_with_ttl(&self, key: u64) -> Result<Option<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造 TTL 键
        // - 目的: 获取对应记录
        let ttl_key = self.ttl_key(key);
        // ### 修改记录 (2026-02-25)
        // - 原因: 可能不存在
        // - 目的: 简化调用方逻辑
        let raw = match self.db.get(ttl_key.as_slice())? {
            Some(value) => value,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要解析过期时间
        // - 目的: 判断是否过期
        if raw.len() < 8 {
            // ### 修改记录 (2026-02-25)
            // - 原因: 数据异常
            // - 目的: 直接视为过期并清理
            self.db.remove(ttl_key)?;
            self.db.flush()?;
            return Ok(None);
        }
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&raw[0..8]);
        let expire_at_ms = u64::from_be_bytes(ts_bytes);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要判断过期
        // - 目的: 过期直接丢弃
        if expire_at_ms <= Self::now_ms() {
            self.db.remove(ttl_key)?;
            self.db.flush()?;
            return Ok(None);
        }
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要返回 payload
        // - 目的: 跳过 TTL 头部
        Ok(Some(raw[8..].to_vec()))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化物理状态序号
    /// - 目的: 确保 HWM 推进有依据
    pub fn persist_state(&self, seq: u64) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化状态序号
        self.db.insert(STATE_SEQ_KEY, seq.to_be_bytes().to_vec())?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可校验
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要推进高水位线
    /// - 目的: 标记已安全执行的最大序号
    pub fn set_hwm(&self, seq: u64) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取物理状态序号
        // - 目的: 避免先推进 HWM
        let state_seq = self
            .db
            .get(STATE_SEQ_KEY)?
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0);
        // ### 修改记录 (2026-02-25)
        // - 原因: HWM 不可超过物理状态
        // - 目的: 防止断电半联动
        if seq > state_seq {
            return Err(anyhow::anyhow!("HWM exceeds persisted state"));
        }
        self.db.insert(HWM_KEY, seq.to_be_bytes().to_vec())?;
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要 ACK 推进高水位线
    /// - 目的: 保证幂等且单调递增
    pub fn ack_hwm(&self, seq: u64) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取物理状态序号
        // - 目的: 避免 ACK 超过落盘状态
        let state_seq = self
            .db
            .get(STATE_SEQ_KEY)?
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0);
        // ### 修改记录 (2026-02-25)
        // - 原因: 不能超过物理状态
        // - 目的: 防止断电半联动
        if seq > state_seq {
            return Err(anyhow::anyhow!("HWM exceeds persisted state"));
        }
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取当前 HWM
        // - 目的: 保证单调递增
        let current = self.get_hwm()?;
        if seq <= current {
            return Ok(());
        }
        self.db.insert(HWM_KEY, seq.to_be_bytes().to_vec())?;
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取高水位线
    /// - 目的: 供重放幂等判断使用
    pub fn get_hwm(&self) -> Result<u64> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 读取持久化值
        // - 目的: 断电恢复可取回
        let value = self.db.get(HWM_KEY)?;
        Ok(value
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0))
    }
}
