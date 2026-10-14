//! Opt-in input-only event watches. This configuration owns watch policy;
//! connection credentials remain owned by the referenced MCP server.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use zeroclaw_macros::Configurable;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.event_watch"]
pub struct EventWatchConfig {
    /// Enable this input-only listener. Requires an enabled agent binding and SOP runtime.
    #[serde(default)]
    pub enabled: bool,
    /// Source adapter: `fastmail` or `home_assistant`.
    #[serde(default)]
    pub source: String,
    /// Existing HTTP MCP server alias. Its URL and credentials are resolved live.
    #[serde(default)]
    pub mcp_server: String,
    /// Exactly one SOP to dispatch. It must declare a matching channel trigger.
    #[serde(default)]
    pub sop: String,
    /// Poll interval in seconds, 10..=3600. Polling does not invoke an LLM.
    #[serde(default = "default_poll")]
    pub poll_interval_secs: u64,
    /// Fastmail search expression; keep it time-independent so the checkpoint owns time.
    #[serde(default = "default_query")]
    pub query: String,
    /// Explicit Home Assistant entity IDs. Empty denies all; wildcards are forbidden.
    #[serde(default)]
    pub entities: Vec<String>,
    /// Trigger when a watched entity enters one of these states.
    #[serde(default)]
    pub states: Vec<String>,
    /// Numeric upper bound; values strictly above it match.
    #[serde(default)]
    pub above: Option<f64>,
    /// Numeric lower bound; values strictly below it match. Both bounds mean outside the range.
    #[serde(default)]
    pub below: Option<f64>,
    /// Condition must remain observed for this duration. Disconnect time does not prove continuity.
    #[serde(default = "default_poll")]
    pub for_secs: u64,
    /// Minimum interval between emitted conditions for one entity.
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,
}

fn default_poll() -> u64 {
    60
}
fn default_cooldown() -> u64 {
    300
}
fn default_query() -> String {
    "in:inbox".into()
}

impl Default for EventWatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source: String::new(),
            mcp_server: String::new(),
            sop: String::new(),
            poll_interval_secs: default_poll(),
            query: default_query(),
            entities: Vec::new(),
            states: Vec::new(),
            above: None,
            below: None,
            for_secs: default_poll(),
            cooldown_secs: default_cooldown(),
        }
    }
}

impl EventWatchConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.source.as_str(), "fastmail" | "home_assistant"),
            "event_watch source must be fastmail or home_assistant"
        );
        ensure!(
            !self.mcp_server.trim().is_empty() && !self.sop.trim().is_empty(),
            "event_watch needs an MCP server and target SOP"
        );
        ensure!(
            (10..=3600).contains(&self.poll_interval_secs),
            "event_watch poll interval must be 10..=3600 seconds"
        );
        ensure!(
            self.for_secs <= 86400 && self.cooldown_secs <= 86400,
            "event_watch durations must not exceed one day"
        );
        ensure!(self.query.len() <= 2048, "event_watch query is too long");
        if self.source == "home_assistant" {
            ensure!(
                !self.entities.is_empty() && self.entities.len() <= 128,
                "event_watch requires 1..=128 explicit entities"
            );
            for entity in &self.entities {
                let (domain, name) = entity.split_once('.').unwrap_or(("", ""));
                ensure!(
                    !domain.is_empty()
                        && !name.is_empty()
                        && domain
                            .bytes()
                            .chain(name.bytes())
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                    "invalid event_watch entity ID"
                );
            }
            let numeric = self.above.is_some() || self.below.is_some();
            ensure!(
                numeric == self.states.is_empty(),
                "event_watch requires either states or numeric bounds"
            );
            ensure!(
                self.states.len() <= 32
                    && self.states.iter().all(|s| !s.is_empty() && s.len() <= 128),
                "invalid event_watch state filter"
            );
            ensure!(
                self.above.into_iter().chain(self.below).all(f64::is_finite),
                "event_watch numeric bounds must be finite"
            );
            if let (Some(above), Some(below)) = (self.above, self.below) {
                ensure!(
                    below < above,
                    "event_watch lower bound must be below upper bound"
                );
            }
        }
        Ok(())
    }

    pub fn matches_state(&self, state: &str) -> bool {
        if !self.states.is_empty() {
            return self.states.iter().any(|s| s == state);
        }
        state
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .is_some_and(|v| {
                self.above.is_some_and(|bound| v > bound)
                    || self.below.is_some_and(|bound| v < bound)
            })
    }
}

impl crate::traits::ChannelConfig for EventWatchConfig {
    fn name() -> &'static str {
        "Event Watch"
    }
    fn desc() -> &'static str {
        "Fastmail and Home Assistant SOP event observers"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_closed_and_round_trip() {
        let cfg: EventWatchConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, EventWatchConfig::default());
        assert!(!cfg.enabled);
    }
    #[test]
    fn requires_scoped_entities_and_a_real_condition() {
        let mut cfg = EventWatchConfig {
            source: "home_assistant".into(),
            mcp_server: "ha".into(),
            sop: "review".into(),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
        cfg.entities = vec!["light.*".into()];
        cfg.states = vec!["unavailable".into()];
        assert!(cfg.validate().is_err());
        cfg.entities = vec!["light.example".into()];
        assert!(cfg.validate().is_ok());
        cfg.above = Some(30.0);
        assert!(cfg.validate().is_err());
        cfg.states.clear();
        cfg.below = Some(35.0);
        assert!(cfg.validate().is_err());
        cfg.below = Some(20.0);
        assert!(cfg.validate().is_ok());
        cfg.poll_interval_secs = 0;
        assert!(cfg.validate().is_err());
    }
}
