use arc_swap::ArcSwap;
use notify::{event::EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub routes: HashMap<String, Route>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_base_delay_ms")]
    pub base_delay_ms: u64,
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,
    #[serde(default)]
    pub max_total_wait_ms: u64,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_retry_status_codes")]
    pub retry_status_codes: Vec<u16>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_delay_ms: default_base_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            max_total_wait_ms: 0,
            connect_timeout_secs: default_connect_timeout_secs(),
            retry_status_codes: default_retry_status_codes(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub target: String,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub base_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_total_wait_ms: Option<u64>,
    #[serde(default)]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default)]
    pub retry_status_codes: Option<Vec<u16>>,
}

/// Resolved route config: route-level overrides merged with defaults.
#[derive(Debug, Clone)]
pub struct ResolvedRouteConfig {
    pub target: String,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub max_total_wait_ms: u64,
    pub connect_timeout_secs: u64,
    pub retry_status_codes: Vec<u16>,
}

fn default_max_retries() -> u32 {
    9999
}
fn default_base_delay_ms() -> u64 {
    1000
}
fn default_max_delay_ms() -> u64 {
    60000
}
fn default_connect_timeout_secs() -> u64 {
    30
}
fn default_retry_status_codes() -> Vec<u16> {
    vec![429, 500, 502, 503, 504, 408, 529]
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            ConfigError(format!(
                "failed to read config file {}: {}",
                path.display(),
                e
            ))
        })?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| ConfigError(format!("failed to parse TOML: {}", e)))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.routes.is_empty() {
            return Err(ConfigError("no routes defined".into()));
        }
        for (name, route) in &self.routes {
            if name.contains('/') {
                return Err(ConfigError(format!(
                    "route name '{}' must not contain '/'",
                    name
                )));
            }
            if route.target.is_empty() {
                return Err(ConfigError(format!("route '{}' has empty target", name)));
            }
            // Validate URL
            if let Err(e) = route.target.parse::<http::Uri>() {
                return Err(ConfigError(format!(
                    "route '{}' target '{}' is not a valid URL: {}",
                    name, route.target, e
                )));
            }
        }
        if self.defaults.max_retries == 0 {
            return Err(ConfigError("defaults.max_retries must be > 0".into()));
        }
        if self.defaults.base_delay_ms == 0 {
            return Err(ConfigError("defaults.base_delay_ms must be > 0".into()));
        }
        if self.defaults.max_delay_ms == 0 {
            return Err(ConfigError("defaults.max_delay_ms must be > 0".into()));
        }
        Ok(())
    }

    pub fn resolve_route(&self, name: &str) -> Option<ResolvedRouteConfig> {
        let route = self.routes.get(name)?;
        let d = &self.defaults;
        Some(ResolvedRouteConfig {
            target: route.target.clone(),
            max_retries: route.max_retries.unwrap_or(d.max_retries),
            base_delay_ms: route.base_delay_ms.unwrap_or(d.base_delay_ms),
            max_delay_ms: route.max_delay_ms.unwrap_or(d.max_delay_ms),
            max_total_wait_ms: route.max_total_wait_ms.unwrap_or(d.max_total_wait_ms),
            connect_timeout_secs: route.connect_timeout_secs.unwrap_or(d.connect_timeout_secs),
            retry_status_codes: route
                .retry_status_codes
                .clone()
                .unwrap_or_else(|| d.retry_status_codes.clone()),
        })
    }

    pub fn route_names(&self) -> Vec<&str> {
        self.routes.keys().map(|s| s.as_str()).collect()
    }
}

/// Watches the config file for changes and hot-reloads into ArcSwap.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    pub fn start(config: Arc<ArcSwap<Config>>, path: PathBuf) -> Result<Self, ConfigError> {
        let watch_path = path.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    return;
                }
                // Debounce: check mtime, wait 100ms, reload
                let path = watch_path.clone();
                let config = config.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    match Config::load(&path) {
                        Ok(new_config) => {
                            let route_names: Vec<String> = new_config
                                .route_names()
                                .into_iter()
                                .map(|s| s.to_string())
                                .collect();
                            config.store(Arc::new(new_config));
                            info!("config reloaded (routes: {})", route_names.join(", "));
                        }
                        Err(e) => {
                            warn!("config reload failed, keeping old config: {}", e);
                        }
                    }
                });
            }
        })
        .map_err(|e| ConfigError(format!("failed to create file watcher: {}", e)))?;

        // Watch the parent directory to catch atomic save (rename)
        let watch_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        watcher
            .watch(&watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e| ConfigError(format!("failed to watch {}: {}", watch_dir.display(), e)))?;

        info!("watching config file: {}", path.display());
        Ok(ConfigWatcher { _watcher: watcher })
    }
}
