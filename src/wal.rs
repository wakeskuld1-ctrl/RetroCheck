use anyhow::Result;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct WalLogger {
    log_file: Arc<Mutex<std::fs::File>>,
}

impl WalLogger {
    pub fn new(path: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            log_file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn log(&self, entry: &str) -> Result<()> {
        let mut file = self.log_file.lock().unwrap();
        writeln!(file, "{}", entry)?;
        Ok(())
    }
}
