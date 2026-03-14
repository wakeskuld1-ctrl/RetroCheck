// ### 修改记录 (2026-03-14)
// - 原因: 需要覆盖 NonceCache 持久化加载后的截断语义
// - 目的: 确保 per-group 上限生效，旧 nonce 会被淘汰
use anyhow::Result;
use check_program::hub::edge_gateway::NonceCache;
use tempfile::tempdir;

#[test]
fn nonce_cache_truncates_on_load() -> Result<()> {
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要可控的持久化目录
    // - 目的: 复现加载 + 截断路径
    let dir = tempdir()?;
    let path = dir.path().join("nonce_cache");

    // ### 修改记录 (2026-03-14)
    // - 原因: 先写入超过上限的 nonce
    // - 目的: 让加载阶段必须执行截断
    {
        let mut cache = NonceCache::new_with_persistence(5, Some(path.to_string_lossy().to_string()))?;
        cache.seen(1, 10)?;
        cache.seen(1, 11)?;
        cache.seen(1, 12)?;
        cache.seen(1, 13)?;
        cache.seen(1, 14)?;
    }

    // ### 修改记录 (2026-03-14)
    // - 原因: 使用更小上限重新加载
    // - 目的: 验证只保留最新的 nonce
    let mut cache = NonceCache::new_with_persistence(2, Some(path.to_string_lossy().to_string()))?;

    // ### 修改记录 (2026-03-14)
    // - 原因: 最新 nonce 仍应被识别为重放
    // - 目的: 确保截断保留最近值
    let err_13 = cache.seen(1, 13).unwrap_err();
    assert!(err_13.to_string().contains("replay detected"));
    let err_14 = cache.seen(1, 14).unwrap_err();
    assert!(err_14.to_string().contains("replay detected"));

    // ### 修改记录 (2026-03-14)
    // - 原因: 旧 nonce 被淘汰后不应判为重放
    // - 目的: 证明加载时截断已生效
    cache.seen(1, 10)?;
    Ok(())
}
