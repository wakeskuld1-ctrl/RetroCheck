use crate::pb::{AddHubRequest, AddHubResponse, NodeCompatibility};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Clone)]
pub struct ClusterBaseline {
    pub app_semver: String,
    pub sqlite_schema_version: u32,
    pub sled_format_version: u32,
    pub log_codec_version: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum AddHubErrorKind {
    InvalidArgument,
    AlreadyExists,
    IncompatibleAppVersion,
    IncompatibleSqliteSchema,
    IncompatibleSledFormat,
    IncompatibleLogCodec,
}

#[derive(Debug)]
pub struct AddHubError {
    kind: AddHubErrorKind,
    message: String,
    pub suggested_action: String,
}

impl AddHubError {
    pub fn invalid_argument(
        message: impl Into<String>,
        suggested_action: impl Into<String>,
    ) -> Self {
        Self {
            kind: AddHubErrorKind::InvalidArgument,
            message: message.into(),
            suggested_action: suggested_action.into(),
        }
    }

    pub fn already_exists(message: impl Into<String>, suggested_action: impl Into<String>) -> Self {
        Self {
            kind: AddHubErrorKind::AlreadyExists,
            message: message.into(),
            suggested_action: suggested_action.into(),
        }
    }

    pub fn incompatible(
        kind: AddHubErrorKind,
        message: impl Into<String>,
        suggested_action: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            suggested_action: suggested_action.into(),
        }
    }

    pub fn reason_code(&self) -> &'static str {
        match self.kind {
            AddHubErrorKind::InvalidArgument => "INVALID_ARGUMENT",
            AddHubErrorKind::AlreadyExists => "NODE_ID_ALREADY_EXISTS",
            AddHubErrorKind::IncompatibleAppVersion => "INCOMPATIBLE_APP_VERSION",
            AddHubErrorKind::IncompatibleSqliteSchema => "INCOMPATIBLE_SQLITE_SCHEMA",
            AddHubErrorKind::IncompatibleSledFormat => "INCOMPATIBLE_SLED_FORMAT",
            AddHubErrorKind::IncompatibleLogCodec => "INCOMPATIBLE_LOG_CODEC",
        }
    }
}

impl Display for AddHubError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AddHubError {}

pub trait CompatibilityChecker: Send + Sync {
    fn check(&self, req: &AddHubRequest) -> std::result::Result<(), AddHubError>;
}

pub trait TopologyService: Send + Sync {
    fn upsert_grpc_addr(&self, node_id: u64, grpc_addr: String);
    fn get_grpc_addr(&self, node_id: u64) -> Option<String>;
}

#[derive(Default)]
pub struct MemoryTopologyService {
    grpc_addrs: RwLock<HashMap<u64, String>>,
}

impl TopologyService for MemoryTopologyService {
    fn upsert_grpc_addr(&self, node_id: u64, grpc_addr: String) {
        if let Ok(mut addrs) = self.grpc_addrs.write() {
            addrs.insert(node_id, normalize_grpc_addr(grpc_addr));
        }
    }

    fn get_grpc_addr(&self, node_id: u64) -> Option<String> {
        self.grpc_addrs
            .read()
            .ok()
            .and_then(|addrs| addrs.get(&node_id).cloned())
    }
}

pub struct SharedFileTopologyService {
    registry_file: PathBuf,
}

impl SharedFileTopologyService {
    pub fn new(registry_file: PathBuf) -> Self {
        Self { registry_file }
    }

    fn read_registry(&self) -> HashMap<u64, String> {
        let text = std::fs::read_to_string(&self.registry_file).unwrap_or_default();
        serde_json::from_str::<HashMap<u64, String>>(&text).unwrap_or_default()
    }

    fn write_registry(&self, registry: &HashMap<u64, String>) {
        let Some(parent) = self.registry_file.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string(registry) else {
            return;
        };
        let _ = std::fs::write(&self.registry_file, text);
    }
}

impl TopologyService for SharedFileTopologyService {
    fn upsert_grpc_addr(&self, node_id: u64, grpc_addr: String) {
        let mut registry = self.read_registry();
        registry.insert(node_id, normalize_grpc_addr(grpc_addr));
        self.write_registry(&registry);
    }

    fn get_grpc_addr(&self, node_id: u64) -> Option<String> {
        self.read_registry().get(&node_id).cloned()
    }
}

pub struct StrictCompatibilityChecker {
    baseline: ClusterBaseline,
}

impl StrictCompatibilityChecker {
    pub fn new(baseline: ClusterBaseline) -> Self {
        Self { baseline }
    }
}

impl CompatibilityChecker for StrictCompatibilityChecker {
    fn check(&self, req: &AddHubRequest) -> std::result::Result<(), AddHubError> {
        let Some(compatibility) = &req.compatibility else {
            return Err(AddHubError::invalid_argument(
                "compatibility is required",
                "include NodeCompatibility in AddHubRequest",
            ));
        };

        if compatibility.app_semver != self.baseline.app_semver {
            return Err(AddHubError::incompatible(
                AddHubErrorKind::IncompatibleAppVersion,
                format!(
                    "app_semver mismatch: got {}, expected {}",
                    compatibility.app_semver, self.baseline.app_semver
                ),
                format!("upgrade or downgrade node app version to {}", self.baseline.app_semver),
            ));
        }

        if compatibility.sqlite_schema_version != self.baseline.sqlite_schema_version {
            return Err(AddHubError::incompatible(
                AddHubErrorKind::IncompatibleSqliteSchema,
                format!(
                    "sqlite_schema_version mismatch: got {}, expected {}",
                    compatibility.sqlite_schema_version, self.baseline.sqlite_schema_version
                ),
                format!(
                    "migrate sqlite schema to version {}",
                    self.baseline.sqlite_schema_version
                ),
            ));
        }

        if compatibility.sled_format_version != self.baseline.sled_format_version {
            return Err(AddHubError::incompatible(
                AddHubErrorKind::IncompatibleSledFormat,
                format!(
                    "sled_format_version mismatch: got {}, expected {}",
                    compatibility.sled_format_version, self.baseline.sled_format_version
                ),
                format!(
                    "migrate sled format to version {}",
                    self.baseline.sled_format_version
                ),
            ));
        }

        if compatibility.log_codec_version != self.baseline.log_codec_version {
            return Err(AddHubError::incompatible(
                AddHubErrorKind::IncompatibleLogCodec,
                format!(
                    "log_codec_version mismatch: got {}, expected {}",
                    compatibility.log_codec_version, self.baseline.log_codec_version
                ),
                format!(
                    "switch log codec to version {}",
                    self.baseline.log_codec_version
                ),
            ));
        }

        Ok(())
    }
}

pub fn default_compatibility(app_semver: String) -> NodeCompatibility {
    NodeCompatibility {
        app_semver,
        sqlite_schema_version: 1,
        sled_format_version: 1,
        log_codec_version: 1,
    }
}

pub fn normalize_grpc_addr(addr: String) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{}", addr)
    }
}

pub fn global_topology_service() -> Arc<dyn TopologyService> {
    static TOPOLOGY_SERVICE: OnceLock<Arc<dyn TopologyService>> = OnceLock::new();
    TOPOLOGY_SERVICE
        .get_or_init(|| {
            let registry_file = std::env::temp_dir().join("check_program").join("topology_registry.json");
            Arc::new(SharedFileTopologyService::new(registry_file))
        })
        .clone()
}

pub fn add_hub_error_response(err: AddHubError) -> AddHubResponse {
    AddHubResponse {
        membership_version: 0,
        leader_hint: String::new(),
        reason_code: err.reason_code().to_string(),
        suggested_action: err.suggested_action,
    }
}

pub fn not_leader_response(leader_hint: String) -> AddHubResponse {
    AddHubResponse {
        membership_version: 0,
        leader_hint,
        reason_code: "NOT_LEADER".to_string(),
        suggested_action: "retry request to leader_hint endpoint".to_string(),
    }
}
