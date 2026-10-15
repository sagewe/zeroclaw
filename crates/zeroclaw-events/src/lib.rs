//! Native event observers, with a legacy input-channel compatibility adapter.
//!
//! No LLM runs in the poll loop. Events enter the shared, untrusted Channel SOP
//! ingress and its generation-owned driver. Live config owns permissions and
//! filters; events.db owns observations and delivery admission, never SOP state.
mod push;
pub mod service;
mod sources;
pub mod store;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use zeroclaw_api::channel::{Channel, ChannelMessage, ChannelSopTopic, SendMessage};
use zeroclaw_config::event_watch::EventWatchConfig;
use zeroclaw_config::integration_plugins::{MailPollSpec, bundled_fastmail};
use zeroclaw_config::live::LiveConfigHandle;
use zeroclaw_config::schema::{McpServerConfig, McpTransport};
use zeroclaw_runtime::sop::SopDriverSink;
use zeroclaw_runtime::sop::audit::SopAuditLogger;
use zeroclaw_runtime::sop::dispatch::{DispatchResult, SopIngress, SopIngressOutcome};
use zeroclaw_runtime::sop::engine::SopEngine;
use zeroclaw_runtime::sop::types::{SopTrigger, SopTriggerSource};

use store::Store;

pub struct EventWatchChannel {
    alias: String,
    live: LiveConfigHandle,
    directory: PathBuf,
    engine: Arc<Mutex<SopEngine>>,
    audit: Arc<SopAuditLogger>,
    driver: SopDriverSink,
}

impl zeroclaw_api::attribution::Attributable for EventWatchChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::EventWatch)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl EventWatchChannel {
    pub fn new(
        alias: String,
        live: LiveConfigHandle,
        directory: PathBuf,
        engine: Arc<Mutex<SopEngine>>,
        audit: Arc<SopAuditLogger>,
        driver: SopDriverSink,
    ) -> Self {
        Self {
            alias,
            live,
            directory,
            engine,
            audit,
            driver,
        }
    }

    /// Resolve grants from the currently published config before every fetch
    /// and again before every dispatch. Never retain a long-lived policy copy.
    fn resolve(
        &self,
    ) -> Result<(
        EventWatchConfig,
        McpServerConfig,
        String,
        Option<MailPollSpec>,
    )> {
        let (watch, native) = {
            let config = self.live.read();
            if config.events.rules.contains_key(&self.alias) {
                (config.event_watch(&self.alias)?, true)
            } else {
                (
                    config
                        .channels
                        .event_watch
                        .get(&self.alias)
                        .cloned()
                        .context("event watch removed")?,
                    false,
                )
            }
        };
        ensure!(watch.enabled, "event watch disabled");
        watch.validate()?;
        let owner = {
            let engine = self.engine.lock().map_err(|_| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_attrs(json!({"alias":self.alias})),
                    "Event watch SOP engine unavailable"
                );
                anyhow::Error::msg("SOP engine unavailable")
            })?;
            let sop = engine
                .get_sop(&watch.sop)
                .context("event watch target SOP missing")?;
            ensure!(sop.triggers.iter().any(|trigger| matches!(trigger,
                SopTrigger::Channel { channel, alias, .. }
                    if channel.eq_ignore_ascii_case("event_watch") && alias.as_deref().is_none_or(|a| a == self.alias))),
                "event watch target SOP needs a matching channel trigger");
            sop.agent
                .clone()
                .context("event watch SOP needs an explicit owning agent")?
        };
        let config = self.live.read();
        ensure!(
            config.sop.runtime_enabled(),
            "event watch requires SOP runtime"
        );
        ensure!(
            if native {
                config.event_watch(&self.alias).ok().as_ref() == Some(&watch)
            } else {
                config.channels.event_watch.get(&self.alias) == Some(&watch)
            },
            "event watch changed during resolution"
        );
        let agent = config
            .agents
            .get(&owner)
            .filter(|a| a.enabled)
            .context("event watch owning agent disabled or missing")?;
        let binding = format!("event_watch.{}", self.alias);
        ensure!(
            native || agent.channels.iter().any(|c| c.as_str() == binding),
            "SOP owner must explicitly bind the event watch"
        );
        let server = config
            .mcp_servers_for_agent(&owner)
            .into_iter()
            .find(|s| s.name == watch.mcp_server)
            .context("event watch MCP server is not granted to its owning agent")?;
        ensure!(config.mcp.enabled, "MCP access disabled");
        ensure!(
            server.transport == McpTransport::Http,
            "event watch requires an HTTP MCP server"
        );
        let mut server = server;
        server.max_response_bytes = Some(
            server
                .max_response_bytes
                .filter(|n| *n > 0)
                .unwrap_or(1024 * 1024)
                .min(1024 * 1024),
        );
        server.tool_timeout_secs = Some(server.tool_timeout_secs.unwrap_or(30).min(30));
        let mail_spec = if watch.source == "fastmail" {
            Some(config.event_mail_spec(&self.alias)?)
        } else {
            None
        };
        if native
            && config.events.sources[&config.events.rules[&self.alias].source].kind == "plugin"
        {
            let spec = mail_spec
                .as_ref()
                .context("integration mail adapter missing")?;
            ensure!(
                config.integration_poll_allowed(&owner, &watch.mcp_server, &spec.tool),
                "integration poll tool must be explicitly auto-approved by the owning agent"
            );
        }
        // Credential/endpoint changes may switch accounts; invalidate the old
        // baseline and queued work without storing the credential in the DB.
        let mut revision = hex::encode(Sha256::digest(serde_json::to_vec(&(
            &watch, &server, &owner,
        ))?));
        // Preserve the existing Fastmail checkpoint during migration when the
        // wire mapping is unchanged. Other approved mappings own a new cursor.
        if let Some(spec) = &mail_spec
            && *spec != bundled_fastmail()?.event
        {
            revision = hex::encode(Sha256::digest(serde_json::to_vec(&(&revision, spec))?));
        }
        Ok((watch, server, revision, mail_spec))
    }

    async fn drain(&self, store: &Store) -> Result<()> {
        for _ in 0..100 {
            let Some(event) = store.next()? else {
                return Ok(());
            };
            let (watch, _, revision, _) = self.resolve()?;
            if event.revision != revision {
                store.mark(&event.id, "superseded")?;
                continue;
            }
            let topic = ChannelSopTopic::build("event_watch", &self.alias, &event.kind);
            let payload = serde_json::to_string(&event)?;
            store.mark(&event.id, "dispatching")?;
            let outcome = SopIngress::new(Some(&self.engine), Some(&self.audit))
                .with_driver_sink(&self.driver)
                .dispatch(
                    SopTriggerSource::Channel,
                    Some(&topic),
                    Some(&payload),
                    Some(&watch.sop),
                    Some((event.id.clone(), false)),
                )
                .await;
            let status = disposition(&outcome);
            let run_id = match &outcome {
                SopIngressOutcome::Dispatched(results) => results.iter().find_map(|r| match r {
                    DispatchResult::Started { run_id, .. } => Some(run_id.as_str()),
                    DispatchResult::Coalesced {
                        existing_run_id, ..
                    } => Some(existing_run_id.as_str()),
                    _ => None,
                }),
                _ => None,
            };
            store.settle(&event.id, status, run_id)?;
            if status == "queued" {
                return Ok(());
            }
            ::zeroclaw_log::record!(INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(json!({"channel":"event_watch", "alias":self.alias, "event_id":event.id, "event_type":event.kind, "disposition":status})),
                "Event watch delivery settled");
        }
        Ok(())
    }
}

impl EventWatchChannel {
    async fn observe_once(&self, store: &Store, checkpoint: &mut Checkpoint) -> Result<()> {
        let (watch, server, revision, mail_spec) = self.resolve()?;
        if checkpoint.revision != revision {
            *checkpoint = Checkpoint {
                revision: revision.clone(),
                ..Checkpoint::default()
            };
        }
        self.drain(store).await?;
        let snapshot = tokio::time::timeout(
            Duration::from_secs(90),
            sources::fetch(&watch, server, checkpoint, mail_spec.as_ref()),
        )
        .await
        .context("event watch poll timed out")??;
        // A disabled/reconfigured watch cannot publish a completed stale read.
        let (_, _, current_revision, _) = self.resolve()?;
        if current_revision != revision {
            return Ok(());
        }
        let mut next = checkpoint.clone();
        let events = next.observe(&watch, snapshot, Utc::now().timestamp_millis())?;
        store.save(&next, &events)?;
        store.poll_succeeded()?;
        *checkpoint = next;
        self.drain(store).await?;
        Ok(())
    }

    async fn listen_push(
        &self,
        store: &Store,
        checkpoint: &mut Checkpoint,
        settings: (zeroclaw_config::integration_plugins::JmapPushSpec, String),
    ) -> Result<()> {
        self.resolve()?;
        self.drain(store).await?;
        let mut connection = push::PushConnection::connect(&settings.0, &settings.1).await?;
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                .with_attrs(json!({"alias": self.alias, "transport":"jmap_sse"})),
            "JMAP push stream connected"
        );
        // Subscribe first, then catch up: changes during the read stay buffered in SSE.
        self.observe_once(store, checkpoint).await?;
        let revision = checkpoint.revision.clone();
        let mut activity = tokio::time::Instant::now();
        let mut policy_tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                biased;
                _ = policy_tick.tick() => {
                    let (_, _, current_revision, _) = self.resolve()?;
                    ensure!(current_revision == revision && self.live.read().event_push(&self.alias)?.as_ref() == Some(&settings), "JMAP push policy changed; reconnect required");
                    ensure!(activity.elapsed() < Duration::from_secs(90), "JMAP push heartbeat timed out");
                    self.drain(store).await?;
                }
                changed = connection.next() => {
                    let changed = changed?;
                    activity = tokio::time::Instant::now();
                    if changed { self.observe_once(store, checkpoint).await?; }
                    store.poll_succeeded()?;
                }
            }
        }
    }
}

fn disposition(outcome: &SopIngressOutcome) -> &'static str {
    match outcome {
        SopIngressOutcome::Dispatched(results) if !results.is_empty() => {
            if results.iter().any(|r| {
                matches!(
                    r,
                    DispatchResult::Started { .. } | DispatchResult::Coalesced { .. }
                )
            }) {
                "delivered"
            } else if results
                .iter()
                .any(|r| matches!(r, DispatchResult::Deferred { .. }))
            {
                "queued"
            } else if results.iter().all(|r| matches!(r, DispatchResult::NoMatch)) {
                "ignored"
            } else {
                "rejected"
            }
        }
        _ => "queued",
    }
}

#[async_trait]
impl Channel for EventWatchChannel {
    fn name(&self) -> &str {
        "event_watch"
    }
    fn supports_outbound_send(&self) -> bool {
        false
    }
    async fn send(&self, _message: &SendMessage) -> Result<()> {
        bail!("event_watch is input-only")
    }
    async fn health_check(&self) -> bool {
        self.resolve().is_ok()
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let store = Store::open(&self.directory)?;
        let mut checkpoint = store.load()?;
        // Restart time is not evidence that a condition stayed continuously true.
        for observation in checkpoint.home.values_mut() {
            if observation.since.is_some() {
                observation.since = Some(Utc::now().timestamp_millis());
            }
        }
        let uncertain = store.uncertain_count()?;
        if uncertain > 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(json!({"alias":self.alias,"uncertain_deliveries":uncertain})),
                "Event watch has ambiguous deliveries requiring operator review; they will not be replayed automatically"
            );
        }
        let push = { self.live.read().event_push(&self.alias)? };
        if let Some(settings) = push {
            return self.listen_push(&store, &mut checkpoint, settings).await;
        }
        loop {
            self.observe_once(&store, &mut checkpoint).await?;
            let (watch, _, _, _) = self.resolve()?;
            tokio::time::sleep(Duration::from_secs(watch.poll_interval_secs)).await;
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Checkpoint {
    revision: String,
    mail_boundary: Option<i64>,
    mail_high_water: Option<i64>,
    mail_seen: BTreeMap<String, i64>,
    home: BTreeMap<String, Observation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Observation {
    state: String,
    matched: bool,
    since: Option<i64>,
    last_emit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingEvent {
    id: String,
    revision: String,
    #[serde(rename = "event_type")]
    kind: String,
    observed_at_ms: i64,
    data: Value,
}

enum Snapshot {
    Home(Vec<(String, String)>),
    Mail(Vec<Value>),
}

impl Checkpoint {
    fn event(&self, kind: &str, now: i64, data: Value) -> Result<PendingEvent> {
        let id = hex::encode(Sha256::digest(serde_json::to_vec(&(
            &self.revision,
            kind,
            now,
            &data,
        ))?));
        Ok(PendingEvent {
            id,
            revision: self.revision.clone(),
            kind: kind.into(),
            observed_at_ms: now,
            data,
        })
    }

    fn observe(
        &mut self,
        watch: &EventWatchConfig,
        snapshot: Snapshot,
        now: i64,
    ) -> Result<Vec<PendingEvent>> {
        let mut events = Vec::new();
        match snapshot {
            Snapshot::Home(states) => {
                for (entity, state) in states {
                    let matched = watch.matches_state(&state);
                    let Some(previous) = self.home.get_mut(&entity) else {
                        self.home.insert(
                            entity,
                            Observation {
                                state,
                                matched,
                                ..Observation::default()
                            },
                        );
                        continue;
                    };
                    if matched && !previous.matched {
                        previous.since = Some(now);
                    }
                    if !matched {
                        previous.since = None;
                    }
                    previous.matched = matched;
                    previous.state = state.clone();
                    if let Some(since) = previous.since {
                        let duration = i64::try_from(watch.for_secs)? * 1000;
                        let cooldown = i64::try_from(watch.cooldown_secs)? * 1000;
                        if now.saturating_sub(since) >= duration
                            && previous
                                .last_emit
                                .is_none_or(|last| now.saturating_sub(last) >= cooldown)
                        {
                            previous.since = None;
                            previous.last_emit = Some(now);
                            events.push(self.event(
                                "condition_entered",
                                now,
                                json!({"entity_id":entity,"state":state,"since_ms":since}),
                            )?);
                        }
                    }
                }
            }
            Snapshot::Mail(mails) => {
                let initial = self.mail_boundary.is_none();
                let boundary = *self.mail_boundary.get_or_insert(now);
                let mut high_water = self.mail_high_water.unwrap_or(now);
                // Admission must use the same retained window as deduplication.
                // Search pages can mix recent and historical messages. Once an
                // old ID is pruned, its timestamp must keep it out on later polls.
                // Freeze the cutoff at the PREVIOUS watermark so a catch-up
                // batch does not skip older, still-unobserved mail in that batch.
                let cutoff = boundary.max(high_water.saturating_sub(sources::MAIL_OVERLAP_MS));
                for mail in mails {
                    let id = mail["id"].as_str().context("mail ID missing")?;
                    let received = mail["received_at_ms"]
                        .as_i64()
                        .context("mail timestamp missing")?;
                    // Reject invalid far-future server timestamps rather than move
                    // the watermark past subsequent real mail.
                    ensure!(
                        received <= now + 300_000,
                        "mail timestamp is too far in the future"
                    );
                    if !initial && received >= cutoff && !self.mail_seen.contains_key(id) {
                        events.push(self.event("email_received", now, mail.clone())?);
                    }
                    self.mail_seen.insert(id.to_string(), received);
                    high_water = high_water.max(received);
                }
                self.mail_high_water = Some(high_water);
                self.mail_seen
                    .retain(|_, received| *received >= high_water - sources::MAIL_OVERLAP_MS);
                ensure!(
                    self.mail_seen.len() <= 10000,
                    "event watch dedup window full"
                );
            }
        }
        Ok(events)
    }
}
