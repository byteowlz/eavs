use config::{Config, ConfigError, File};
use serde::Deserialize;
use std::collections::HashMap;

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub upstream: HashMap<String, UpstreamConfig>,
    pub logging: LoggingConfig,
    pub analysis: AnalysisConfig,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct UpstreamConfig {
    #[serde(rename = "type")]
    pub type_: String,
    pub api_key: String,
    pub base_url: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    pub sink: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct AnalysisConfig {
    pub enabled: bool,
    pub broadcast_channel_size: usize,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let s = Config::builder()
            .add_source(File::with_name("config"))
            .build()?;

        s.try_deserialize()
    }
}

pub fn get_api_key(config_key: &str) -> String {
    if config_key.starts_with("env:") {
        let var_name = &config_key[4..];
        std::env::var(var_name).unwrap_or_else(|_| "".to_string())
    } else {
        config_key.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_get_api_key_raw() {
        let key = "sk-12345";
        assert_eq!(get_api_key(key), "sk-12345");
    }

    #[test]
    fn test_get_api_key_env() {
        env::set_var("TEST_API_KEY", "secret-value");
        assert_eq!(get_api_key("env:TEST_API_KEY"), "secret-value");
        env::remove_var("TEST_API_KEY");
    }

    #[test]
    fn test_get_api_key_env_missing() {
        assert_eq!(get_api_key("env:NON_EXISTENT_VAR"), "");
    }
}
