use anyhow::Result;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct WalLogger {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要保存 WAL 路径
    /// - 目的: 支持轮转与重新打开
    path: String,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要持有日志文件
    /// - 目的: 保持追加写
    log_file: Arc<Mutex<std::fs::File>>,
}

impl WalLogger {
    pub fn new(path: &str) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            // ### 修改记录 (2026-02-17)
            // - 原因: 需要保留路径用于轮转
            // - 目的: 支持快照后的日志管理
            path: path.to_string(),
            log_file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn log(&self, entry: &str) -> Result<()> {
        let mut file = self.log_file.lock().unwrap();
        writeln!(file, "{}", entry)?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 快照完成后需要切换 WAL 文件
    /// - 目的: 控制日志膨胀
    pub fn rotate(&self) -> Result<()> {
        let rotated_path = format!("{}.rotated", self.path);
        if std::path::Path::new(&rotated_path).exists() {
            std::fs::remove_file(&rotated_path)?;
        }
        let mut file = self.log_file.lock().unwrap();
        file.sync_all()?;
        match std::fs::rename(&self.path, &rotated_path) {
            Ok(_) => {
                let new_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?;
                *file = new_file;
            }
            Err(_) => {
                std::fs::copy(&self.path, &rotated_path)?;
                file.set_len(0)?;
                file.seek(SeekFrom::End(0))?;
            }
        }
        Ok(())
    }
}
