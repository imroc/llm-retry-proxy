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

/// Model-level override configuration.
///
/// All fields are optional — only specified fields override the route-level config.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub target: Option<String>,
    /// Protocol transform: "responses_to_chat" or "none" to explicitly disable.
    #[serde(default)]
    pub transform: Option<String>,
    /// Rewrite the `model` field in the request body before forwarding upstream.
    #[serde(default)]
    pub upstream_model: Option<String>,
    /// Rewrite the `model` field in the response back to the client's original model name.
    #[serde(default)]
    pub rewrite_response_model: Option<bool>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub target: String,
    /// Protocol transform: "responses_to_chat" converts /v1/responses → /v1/chat/completions.
    /// Use "none" to explicitly disable a route-level transform for specific models.
    #[serde(default)]
    pub transform: Option<String>,
    /// Rewrite the `model` field in the request body before forwarding upstream.
    #[serde(default)]
    pub upstream_model: Option<String>,
    /// Rewrite the `model` field in the response back to the client's original model name.
    #[serde(default)]
    pub rewrite_response_model: Option<bool>,
    /// Model-level overrides: keyed by the model name extracted from the request body.
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
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
/// Call `resolve_model()` to further apply model-level overrides.
#[derive(Debug, Clone)]
pub struct ResolvedRouteConfig {
    pub target: String,
    pub transform: Option<String>,
    pub upstream_model: Option<String>,
    pub rewrite_response_model: bool,
    pub models: HashMap<String, ModelConfig>,
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

/// Normalize transform value: "none" → None (explicit disable).
fn normalize_transform(t: &Option<String>) -> Option<String> {
    match t {
        Some(s) if s == "none" => None,
        Some(s) => Some(s.clone()),
        None => None,
    }
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
            // Validate model-level configs
            for (model_name, mc) in &route.models {
                if model_name.is_empty() {
                    return Err(ConfigError(format!(
                        "route '{}' has a model entry with empty name",
                        name
                    )));
                }
                if let Some(ref target) = mc.target {
                    if target.is_empty() {
                        return Err(ConfigError(format!(
                            "route '{}' model '{}' has empty target",
                            name, model_name
                        )));
                    }
                    if let Err(e) = target.parse::<http::Uri>() {
                        return Err(ConfigError(format!(
                            "route '{}' model '{}' target '{}' is not a valid URL: {}",
                            name, model_name, target, e
                        )));
                    }
                }
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
            transform: normalize_transform(&route.transform),
            upstream_model: route.upstream_model.clone(),
            rewrite_response_model: route.rewrite_response_model.unwrap_or(false),
            models: route.models.clone(),
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

impl ResolvedRouteConfig {
    /// Apply model-level overrides on top of the resolved route config.
    ///
    /// If the model is not found in the models map, returns a clone of self unchanged.
    pub fn resolve_model(&self, model: &str) -> ResolvedRouteConfig {
        let Some(mc) = self.models.get(model) else {
            return self.clone();
        };
        let mut result = self.clone();
        if let Some(t) = &mc.target {
            result.target = t.clone();
        }
        if let Some(t) = &mc.transform {
            result.transform = normalize_transform(&Some(t.clone()));
        }
        if let Some(v) = &mc.upstream_model {
            result.upstream_model = Some(v.clone());
        }
        if let Some(v) = mc.rewrite_response_model {
            result.rewrite_response_model = v;
        }
        if let Some(v) = mc.max_retries {
            result.max_retries = v;
        }
        if let Some(v) = mc.base_delay_ms {
            result.base_delay_ms = v;
        }
        if let Some(v) = mc.max_delay_ms {
            result.max_delay_ms = v;
        }
        if let Some(v) = mc.max_total_wait_ms {
            result.max_total_wait_ms = v;
        }
        if let Some(v) = mc.connect_timeout_secs {
            result.connect_timeout_secs = v;
        }
        if let Some(v) = &mc.retry_status_codes {
            result.retry_status_codes = v.clone();
        }
        result
    }

    /// List model names configured for this route.
    pub fn model_names(&self) -> Vec<&str> {
        self.models.keys().map(|s| s.as_str()).collect()
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
                            let route_names = new_config.route_names();
                            info!("config reloaded: routes = {}", route_names.join(", "));
                            config.store(Arc::new(new_config));
                        }
                        Err(e) => {
                            warn!("config reload failed, keeping old config: {}", e);
                        }
                    }
                });
            }
        })
        .map_err(|e| ConfigError(format!("failed to create file watcher: {}", e)))?;

        watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|e| ConfigError(format!("failed to watch config file: {}", e)))?;

        Ok(Self { _watcher: watcher })
    }
}
