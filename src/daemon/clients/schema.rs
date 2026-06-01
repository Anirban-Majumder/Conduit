//! Config spec (compile-time `ClientDef` data) and config schema (runtime DB form).
//!
//! `ConfigSpec` lives in each `ClientDef` with `&'static` lifetimes.
//! `ConfigSchema` is the owned, serde-ready form stored in `client.schema_json`.
//! Conversion: `ConfigSchema::from(&spec)`.

use serde::{Deserialize, Serialize};

/// MCP config file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    #[default]
    Json,
    Jsonc,
    Toml,
}

/// Static config writing spec — lives in `ClientDef`, compile-time.
#[derive(Debug, Clone, Copy)]
pub struct ConfigSpec {
    pub format: ConfigFormat,
    pub key_path: &'static [&'static str],
    /// JSON literal with `${url}` (and optionally `${name}`, `${key}`, `${port}`)
    /// placeholders, parsed at render time.
    pub entry_template: &'static str,
}

/// Runtime config schema — serialized into `client.schema_json` in the DB.
///
/// All fields have `#[serde(default)]` so a partial/empty JSON object
/// deserializes without error (forward-compat for schema additions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConfigSchema {
    #[serde(default)]
    pub format: ConfigFormat,
    #[serde(default)]
    pub key_path: Vec<String>,
    #[serde(default)]
    pub entry_template: serde_json::Value,
}

impl From<&ConfigSpec> for ConfigSchema {
    fn from(spec: &ConfigSpec) -> Self {
        Self {
            format: spec.format,
            key_path: spec.key_path.iter().map(|s| (*s).to_owned()).collect(),
            // entry_template is a JSON literal string — parse it into a Value.
            // If it's not valid JSON (shouldn't happen for built-ins), store as a string Value.
            entry_template: serde_json::from_str(spec.entry_template)
                .unwrap_or_else(|_| serde_json::Value::String(spec.entry_template.to_owned())),
        }
    }
}
