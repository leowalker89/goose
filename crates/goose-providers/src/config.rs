use etcetera::AppStrategy;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_yaml::Mapping;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderConfigError {
    #[error("Configuration value not found: {0}")]
    NotFound(String),
    #[error("Failed to deserialize configuration value: {0}")]
    Deserialize(String),
    #[error("Configuration storage error: {0}")]
    Storage(String),
}

impl From<serde_json::Error> for ProviderConfigError {
    fn from(err: serde_json::Error) -> Self {
        ProviderConfigError::Deserialize(err.to_string())
    }
}

impl From<serde_yaml::Error> for ProviderConfigError {
    fn from(err: serde_yaml::Error) -> Self {
        ProviderConfigError::Deserialize(err.to_string())
    }
}

pub trait ProviderConfigStore: Send + Sync {
    fn get_param_value(&self, key: &str) -> Result<Value, ProviderConfigError>;
}

pub trait ProviderConfigExt {
    fn get_param<T: DeserializeOwned>(&self, key: &str) -> Result<T, ProviderConfigError>;
}

impl<T> ProviderConfigExt for T
where
    T: ProviderConfigStore + ?Sized,
{
    fn get_param<U: DeserializeOwned>(&self, key: &str) -> Result<U, ProviderConfigError> {
        let value = self.get_param_value(key)?;
        match serde_json::from_value(value.clone()) {
            Ok(value) => Ok(value),
            Err(json_err) => {
                let Some(string_value) = value.as_str() else {
                    return Err(ProviderConfigError::Deserialize(json_err.to_string()));
                };
                serde_json::from_value(parse_env_value(string_value))
                    .map_err(|_| ProviderConfigError::Deserialize(json_err.to_string()))
            }
        }
    }
}

#[derive(Default)]
pub struct DefaultProviderConfig;

pub fn default_provider_config_store() -> Arc<dyn ProviderConfigStore> {
    Arc::new(DefaultProviderConfig)
}

impl ProviderConfigStore for DefaultProviderConfig {
    fn get_param_value(&self, key: &str) -> Result<Value, ProviderConfigError> {
        let env_key = key.to_uppercase();
        if let Ok(value) = env::var(&env_key) {
            return Ok(parse_env_value(&value));
        }

        let values = load_config_values()?;
        let value = values
            .get(key)
            .ok_or_else(|| ProviderConfigError::NotFound(key.to_string()))?;

        match serde_yaml::from_value(value.clone()) {
            Ok(value) => Ok(value),
            Err(yaml_err) => {
                let Some(string_value) = value.as_str() else {
                    return Err(yaml_err.into());
                };
                Ok(parse_env_value(string_value))
            }
        }
    }
}

fn parse_env_value(value: &str) -> Value {
    if let Ok(json_value) = serde_json::from_str(value) {
        return json_value;
    }

    let trimmed = value.trim();
    match trimmed.to_lowercase().as_str() {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }

    if let Ok(int_value) = trimmed.parse::<i64>() {
        return Value::Number(int_value.into());
    }

    if let Ok(float_value) = trimmed.parse::<f64>() {
        if let Some(number) = serde_json::Number::from_f64(float_value) {
            return Value::Number(number);
        }
    }

    Value::String(value.to_string())
}

fn load_config_values() -> Result<Mapping, ProviderConfigError> {
    let mut merged = Mapping::new();
    for path in config_paths() {
        if !path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| ProviderConfigError::Storage(e.to_string()))?;
        let values: Mapping = serde_yaml::from_str(&content)?;
        merge_config_values(&mut merged, values);
    }
    Ok(merged)
}

fn merge_config_values(base: &mut Mapping, overlay: Mapping) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (
                Some(serde_yaml::Value::Mapping(base_map)),
                serde_yaml::Value::Mapping(overlay_map),
            ) => {
                merge_config_values(base_map, overlay_map);
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn config_paths() -> Vec<PathBuf> {
    let config_dir = config_dir();
    let mut paths = vec![system_config_path()];
    paths.extend(additional_config_paths_from_env());
    paths.push(config_dir.join("config.yaml"));
    paths
}

fn additional_config_paths_from_env() -> Vec<PathBuf> {
    env::var_os("GOOSE_ADDITIONAL_CONFIG_FILES")
        .map(|value| env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn system_config_path() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/etc/goose/config.yaml")
    }
    #[cfg(windows)]
    {
        env::var("PROGRAMDATA")
            .map(|dir| PathBuf::from(dir).join("goose").join("config.yaml"))
            .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData\goose\config.yaml"))
    }
}

fn config_dir() -> PathBuf {
    if let Ok(test_root) = env::var("GOOSE_PATH_ROOT") {
        return PathBuf::from(test_root).join("config");
    }

    let strategy = etcetera::choose_app_strategy(etcetera::AppStrategyArgs {
        top_level_domain: "Block".to_string(),
        author: "Block".to_string(),
        app_name: "goose".to_string(),
    })
    .expect("goose requires a home dir");

    strategy.config_dir()
}
