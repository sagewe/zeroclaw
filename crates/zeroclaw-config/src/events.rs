//! Event source and trigger policy. Credentials stay in the referenced MCP server.
use crate::event_watch::EventWatchConfig;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroclaw_macros::Configurable;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct EventsConfig {
    #[nested]
    pub sources: HashMap<String, EventSource>,
    #[nested]
    pub rules: HashMap<String, EventRule>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct EventSource {
    pub enabled: bool,
    pub kind: String,
    pub mcp_server: String,
    pub poll_interval_secs: u64,
    pub query: String,
    pub entities: Vec<String>,
}
impl Default for EventSource {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: "fastmail".into(),
            mcp_server: String::new(),
            poll_interval_secs: 60,
            query: "in:inbox".into(),
            entities: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct EventRule {
    pub enabled: bool,
    pub source: String,
    pub sop: String,
    pub states: Vec<String>,
    pub above: Option<f64>,
    pub below: Option<f64>,
    pub for_secs: u64,
    pub cooldown_secs: u64,
}
impl Default for EventRule {
    fn default() -> Self {
        Self {
            enabled: false,
            source: String::new(),
            sop: String::new(),
            states: Vec::new(),
            above: None,
            below: None,
            for_secs: 60,
            cooldown_secs: 300,
        }
    }
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
impl EventsConfig {
    /// On-demand view for the observation algorithm, never another policy store.
    pub fn watch(&self, alias: &str) -> Result<EventWatchConfig> {
        let rule = self
            .rules
            .get(alias)
            .ok_or_else(|| anyhow::Error::msg("event rule missing"))?;
        let source = self
            .sources
            .get(&rule.source)
            .ok_or_else(|| anyhow::Error::msg("event source missing"))?;
        Ok(EventWatchConfig {
            enabled: rule.enabled && source.enabled,
            source: source.kind.clone(),
            mcp_server: source.mcp_server.clone(),
            sop: rule.sop.clone(),
            poll_interval_secs: source.poll_interval_secs,
            query: source.query.clone(),
            entities: source.entities.clone(),
            states: rule.states.clone(),
            above: rule.above,
            below: rule.below,
            for_secs: rule.for_secs,
            cooldown_secs: rule.cooldown_secs,
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.sources.len() <= 64 && self.rules.len() <= 128,
            "too many event sources or rules"
        );
        for (id, source) in &self.sources {
            ensure!(valid_id(id), "invalid event source id");
            ensure!(
                matches!(source.kind.as_str(), "fastmail" | "home_assistant"),
                "unsupported event source kind"
            );
            ensure!(
                !source.mcp_server.is_empty(),
                "event source needs a connection"
            );
            ensure!(
                (10..=3600).contains(&source.poll_interval_secs),
                "poll interval must be 10..=3600 seconds"
            );
            ensure!(source.query.len() <= 2048, "event query too long");
        }
        for id in self.rules.keys() {
            ensure!(valid_id(id), "invalid event rule id");
            self.watch(id)?.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_and_path_ids_fail_closed() {
        let mut config = EventsConfig::default();
        config.rules.insert(
            "mail".into(),
            EventRule {
                source: "missing".into(),
                sop: "review".into(),
                ..Default::default()
            },
        );
        assert!(config.validate().is_err());
        config.sources.insert(
            "missing".into(),
            EventSource {
                mcp_server: "mail".into(),
                ..Default::default()
            },
        );
        assert!(config.validate().is_ok());
        assert!(!config.watch("mail").unwrap().enabled);
        let rule = config.rules.remove("mail").unwrap();
        config.rules.insert("../escape".into(), rule);
        assert!(config.validate().is_err());
    }
}
