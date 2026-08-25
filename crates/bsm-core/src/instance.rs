use crate::{BsmError, BsmResult, types::InstanceId};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::fs::{OpenOptions, remove_file};
use std::path::PathBuf;

/// Instance lifecycle states
#[derive(Debug, Clone)]
pub enum InstanceState {
    Created,
    Running,
    Stopped,
}

/// Lightweight instance descriptor
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: InstanceId,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub state: InstanceState,
}

impl Instance {
    pub fn new(name: Option<String>) -> Self {
        let now = Utc::now();
        let nanos: i128 = (now.timestamp() as i128) * 1_000_000_000i128 + now.timestamp_subsec_nanos() as i128;
        let id = InstanceId::from(format!("inst-{}", nanos));
        Instance {
            id,
            name,
            created_at: Utc::now(),
            state: InstanceState::Created,
        }
    }
}

/// A simple manager for Instances. Thread-safe.
#[derive(Clone)]
pub struct InstanceManager {
    inner: Arc<Mutex<HashMap<String, Instance>>>,
}

impl InstanceManager {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn create(&self, name: Option<String>) -> BsmResult<InstanceId> {
        let mut guard = self.inner.lock().map_err(|e| BsmError::Unknown(format!("mutex poisoned: {:?}", e)))?;
        let inst = Instance::new(name);
        let id = inst.id.clone();
        guard.insert(id.0.clone(), inst);
        Ok(id)
    }

    pub fn get(&self, id: &InstanceId) -> BsmResult<Option<Instance>> {
        let guard = self.inner.lock().map_err(|e| BsmError::Unknown(format!("mutex poisoned: {:?}", e)))?;
        Ok(guard.get(&id.0).cloned())
    }

    pub fn list(&self) -> BsmResult<Vec<Instance>> {
        let guard = self.inner.lock().map_err(|e| BsmError::Unknown(format!("mutex poisoned: {:?}", e)))?;
        Ok(guard.values().cloned().collect())
    }

    pub fn destroy(&self, id: &InstanceId) -> BsmResult<()> {
        let mut guard = self.inner.lock().map_err(|e| BsmError::Unknown(format!("mutex poisoned: {:?}", e)))?;
        match guard.remove(&id.0) {
            Some(_) => Ok(()),
            None => Err(BsmError::Unknown(format!("instance not found: {}", id.0))),
        }
    }
}

/// Single-instance guard implemented by creating a lock file in the temp dir.
/// This is a cross-platform lightweight approach; on Windows a named mutex
/// would be preferable (can be added later).
pub struct SingleInstanceGuard {
    lock_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("Another instance of Baxter's Stereo Mix is already running")]
    AlreadyRunning,
    #[error("System error acquiring instance guard: {0}")]
    SystemError(String),
}

impl SingleInstanceGuard {
    pub fn acquire() -> Result<Self, InstanceError> {
        let mut path = std::env::temp_dir();
        path.push("bsm_single_instance.lock");

        // Try to create the file exclusively.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_f) => Ok(Self { lock_path: path }),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Err(InstanceError::AlreadyRunning)
                } else {
                    Err(InstanceError::SystemError(format!("{}", e)))
                }
            }
        }
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        let _ = remove_file(&self.lock_path);
    }
}
