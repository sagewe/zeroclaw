//! Home Assistant's filtered entity subscription provides an ordered initial snapshot
//! and deltas. Checkpoint.home is the only retained device state; attributes are dropped.
use super::{Checkpoint, EventWatchChannel, Snapshot, store::Store};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use zeroclaw_config::{
    event_watch::EventWatchConfig, integration_plugins::EventPushConfig, schema::McpServerConfig,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const MAX_MESSAGE: usize = 1024 * 1024;
struct HomeConnection {
    socket: Socket,
}

fn endpoint(server: &McpServerConfig) -> Result<url::Url> {
    let mut url = url::Url::parse(
        server
            .url
            .as_deref()
            .context("Home Assistant URL missing")?,
    )?;
    ensure!(
        url.path() == "/api/mcp"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid Home Assistant MCP endpoint"
    );
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => anyhow::bail!("invalid Home Assistant scheme"),
    };
    url.set_scheme(scheme)
        .map_err(|()| anyhow::Error::msg("invalid WebSocket scheme"))?;
    url.set_path("/api/websocket");
    Ok(url)
}
impl HomeConnection {
    async fn connect(
        watch: &EventWatchConfig,
        server: &McpServerConfig,
    ) -> Result<(Self, Vec<(String, String)>)> {
        // Empty entity_ids means all entities to HA, so this must fail before sending anything.
        watch.validate()?;
        ensure!(
            watch.source == "home_assistant" && !watch.entities.is_empty(),
            "Home Assistant requires an explicit entity scope"
        );
        tokio::time::timeout(Duration::from_secs(30), Self::connect_inner(watch, server))
            .await
            .context("Home Assistant push setup timed out")?
    }
    async fn connect_inner(
        watch: &EventWatchConfig,
        server: &McpServerConfig,
    ) -> Result<(Self, Vec<(String, String)>)> {
        let url = endpoint(server)?;
        let token = server
            .headers
            .get("Authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .filter(|s| !s.is_empty())
            .context("Home Assistant bearer credential missing")?;
        let limits = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (socket, _) =
            tokio_tungstenite::connect_async_with_config(url.as_str(), Some(limits), false)
                .await
                .context("Home Assistant WebSocket connection failed")?;
        let mut connection = Self { socket };
        ensure!(
            connection.next().await?["type"] == "auth_required",
            "Home Assistant authentication challenge missing"
        );
        connection
            .send(json!({"type":"auth", "access_token":token}))
            .await?;
        ensure!(
            connection.next().await?["type"] == "auth_ok",
            "Home Assistant authentication rejected"
        );
        connection
            .send(json!({"id":1,"type":"subscribe_entities","entity_ids":watch.entities}))
            .await?;
        let ack = connection.next().await?;
        ensure!(
            ack["type"] == "result" && ack["id"] == 1 && ack["success"] == true,
            "Home Assistant entity subscription rejected"
        );
        let initial = connection.next().await?;
        ensure!(
            initial["type"] == "event" && initial["id"] == 1 && initial["event"]["a"].is_object(),
            "Home Assistant initial entity snapshot missing"
        );
        let mut states = project(&initial, &watch.entities)?;
        for entity in &watch.entities {
            if !states.iter().any(|(id, _)| id == entity) {
                states.push((entity.clone(), "unavailable".into()));
            }
        }
        Ok((connection, states))
    }
    async fn send(&mut self, value: Value) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.socket.send(Message::Text(value.to_string().into())),
        )
        .await
        .context("Home Assistant push write timed out")?
        .context("Home Assistant push write failed")
    }
    async fn next(&mut self) -> Result<Value> {
        loop {
            let frame = self
                .socket
                .next()
                .await
                .context("Home Assistant push closed")?
                .context("Home Assistant push read failed")?;
            match frame {
                Message::Text(text) => {
                    return serde_json::from_str(&text).context("invalid Home Assistant push JSON");
                }
                Message::Ping(_) => {
                    self.socket
                        .flush()
                        .await
                        .context("Home Assistant pong failed")?;
                }
                Message::Pong(_) => return Ok(json!({"type":"transport_activity"})),
                Message::Close(_) => anyhow::bail!("Home Assistant push disconnected"),
                _ => anyhow::bail!("unexpected Home Assistant frame"),
            }
        }
    }
}

fn state(value: &Value) -> Result<String> {
    let text = value.as_str().context("invalid Home Assistant state")?;
    ensure!(
        !text.is_empty() && text.len() <= 128,
        "Home Assistant state exceeds limit"
    );
    Ok(text.to_owned())
}
fn project(frame: &Value, entities: &[String]) -> Result<Vec<(String, String)>> {
    let mut states = Vec::new();
    if frame["type"] != "event" || frame["id"] != 1 {
        return Ok(states);
    }
    let event = &frame["event"];
    for entity in entities {
        if let Some(value) = event["a"].get(entity) {
            states.push((entity.clone(), state(&value["s"])?));
        }
        if let Some(value) = event["c"][entity]["+"].get("s") {
            states.push((entity.clone(), state(value)?));
        }
        if event["r"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(entity)))
        {
            states.push((entity.clone(), "unavailable".into()));
        }
    }
    Ok(states)
}
impl Checkpoint {
    pub(super) fn reset_home_continuity(&mut self, now: i64) {
        for observation in self.home.values_mut() {
            if observation.since.is_some() {
                observation.since = Some(now);
            }
        }
    }
    /// A scheduling projection only. observe() remains the condition/cooldown authority.
    fn home_due(&self, watch: &EventWatchConfig, now: i64) -> bool {
        self.home.values().any(|observation| {
            observation.since.is_some_and(|since| {
                now.saturating_sub(since) >= (watch.for_secs as i64) * 1000
                    && observation.last_emit.is_none_or(|last| {
                        now.saturating_sub(last) >= (watch.cooldown_secs as i64) * 1000
                    })
            })
        })
    }
}
impl EventWatchChannel {
    fn home_policy(&self, expected: &str) -> Result<EventWatchConfig> {
        let (watch, _, revision, _) = self.resolve()?;
        ensure!(
            revision == expected
                && self.live.read().event_push(&self.alias)?
                    == Some(EventPushConfig::HomeAssistant),
            "Home Assistant push policy changed"
        );
        Ok(watch)
    }
    async fn accept_home(
        &self,
        store: &Store,
        checkpoint: &mut Checkpoint,
        expected: &str,
        states: Vec<(String, String)>,
    ) -> Result<()> {
        let watch = self.home_policy(expected)?;
        let mut next = checkpoint.clone();
        let events = next.observe(
            &watch,
            Snapshot::Home(states),
            Utc::now().timestamp_millis(),
        )?;
        store.save(&next, &events)?;
        *checkpoint = next;
        self.drain(store).await
    }
    pub(super) async fn listen_home_push(
        &self,
        store: &Store,
        checkpoint: &mut Checkpoint,
    ) -> Result<()> {
        let (watch, server, revision, _) = self.resolve()?;
        if checkpoint.revision != revision {
            *checkpoint = Checkpoint {
                revision: revision.clone(),
                ..Default::default()
            };
        }
        self.drain(store).await?;
        let (mut connection, initial) = HomeConnection::connect(&watch, &server).await?;
        // Only time after the new subscription is established proves continuity.
        checkpoint.reset_home_continuity(Utc::now().timestamp_millis());
        self.accept_home(store, checkpoint, &revision, initial)
            .await?;
        store.poll_succeeded()?;
        zeroclaw_log::record!(INFO, zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(json!({"alias":self.alias,"transport":"home_assistant_websocket","entities":watch.entities.len()})), "Home Assistant push stream connected");
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut last_pong = tokio::time::Instant::now();
        let mut pending_ping: Option<(u64, tokio::time::Instant)> = None;
        let mut next_id: u64 = 2;
        loop {
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    let watch = self.home_policy(&revision)?;
                    self.drain(store).await?;
                    if let Some((_, sent)) = pending_ping {
                        ensure!(sent.elapsed() < Duration::from_secs(15), "Home Assistant pong timed out");
                    } else if last_pong.elapsed() >= Duration::from_secs(30) || checkpoint.home_due(&watch,Utc::now().timestamp_millis()) {
                        connection.send(json!({"id":next_id,"type":"ping"})).await?;
                        pending_ping = Some((next_id,tokio::time::Instant::now()));
                        next_id = next_id.checked_add(1).context("Home Assistant request id exhausted")?;
                    }
                },
                message = connection.next() => {
                    let message = message?;
                    let watch = self.home_policy(&revision)?;
                    if message["type"] == "pong" {
                        ensure!(pending_ping.is_some_and(|(id,_)|message["id"].as_u64()==Some(id)), "unexpected Home Assistant pong");
                        pending_ping = None;
                        last_pong = tokio::time::Instant::now();
                        // HA processes commands and state updates in order. Verify liveness
                        // before maturing a duration, using only the canonical checkpoint.
                        if checkpoint.home_due(&watch,Utc::now().timestamp_millis()) {
                            let states = watch.entities.iter().filter_map(|id|checkpoint.home.get(id).map(|s|(id.clone(),s.state.clone()))).collect();
                            self.accept_home(store, checkpoint, &revision, states).await?;
                        }
                        store.poll_succeeded()?;
                    } else if message["type"] == "event" {
                        let states = project(&message,&watch.entities)?;
                        if !states.is_empty() { self.accept_home(store, checkpoint, &revision, states).await?; }
                        store.poll_succeeded()?;
                    } else if message["type"] != "transport_activity" { anyhow::bail!("unexpected Home Assistant push response"); }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_is_scoped_and_drops_attributes_and_foreign_entities() {
        let scope = vec!["light.fixture".into()];
        let a = json!({"id":1,"type":"event","event":{"a":{"light.fixture":{"s":"off","a":{"private":"do not retain"}},"camera.private":{"s":"on"}}}});
        assert_eq!(
            project(&a, &scope).unwrap(),
            vec![("light.fixture".into(), "off".into())]
        );
        let c = json!({"id":1,"type":"event","event":{"c":{"light.fixture":{"+":{"s":"unavailable"}}}}});
        assert_eq!(project(&c, &scope).unwrap()[0].1, "unavailable");
        let attributes = json!({"id":1,"type":"event","event":{"c":{"light.fixture":{"+":{"a":{"brightness":100}}}}}});
        assert!(project(&attributes, &scope).unwrap().is_empty());
        let r = json!({"id":1,"type":"event","event":{"r":["light.fixture"]}});
        assert_eq!(project(&r, &scope).unwrap()[0].1, "unavailable");
    }
    #[test]
    fn endpoint_stays_on_the_configured_host_and_rejects_credentials_in_url() {
        let mut cfg = McpServerConfig {
            url: Some("https://example.invalid:8123/api/mcp".into()),
            ..Default::default()
        };
        assert_eq!(
            endpoint(&cfg).unwrap().as_str(),
            "wss://example.invalid:8123/api/websocket"
        );
        cfg.url = Some("https://user:pass@example.invalid/api/mcp".into());
        assert!(endpoint(&cfg).is_err());
    }
    #[test]
    fn duration_scheduler_uses_checkpoint_state_and_cooldown() {
        let watch = EventWatchConfig {
            source: "home_assistant".into(),
            mcp_server: "fixture".into(),
            sop: "review".into(),
            entities: vec!["light.fixture".into()],
            states: vec!["unavailable".into()],
            for_secs: 2,
            cooldown_secs: 10,
            ..Default::default()
        };
        let mut cp = Checkpoint::default();
        cp.observe(
            &watch,
            Snapshot::Home(vec![("light.fixture".into(), "on".into())]),
            0,
        )
        .unwrap();
        cp.observe(
            &watch,
            Snapshot::Home(vec![("light.fixture".into(), "unavailable".into())]),
            1000,
        )
        .unwrap();
        assert!(!cp.home_due(&watch, 2999));
        assert!(cp.home_due(&watch, 3000));
        let mut reconnected = cp.clone();
        reconnected.reset_home_continuity(20000);
        assert!(!reconnected.home_due(&watch, 21999));
        assert!(reconnected.home_due(&watch, 22000));
        let events = cp
            .observe(
                &watch,
                Snapshot::Home(vec![("light.fixture".into(), "unavailable".into())]),
                3000,
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(!cp.home_due(&watch, 20000));
        cp.reset_home_continuity(30000);
        assert!(!cp.home_due(&watch, 60000));
    }
}
