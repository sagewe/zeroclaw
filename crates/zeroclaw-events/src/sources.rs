use anyhow::{Context, Result, ensure};
use chrono::DateTime;
use serde_json::{Value, json};
use zeroclaw_config::event_watch::EventWatchConfig;
use zeroclaw_config::schema::McpServerConfig;
use zeroclaw_tools::mcp_client::McpServer;

use super::{Checkpoint, Snapshot};

const MAX_BODY: usize = 1024 * 1024;
pub(super) const MAIL_OVERLAP_MS: i64 = 300_000;

use async_trait::async_trait;
use zeroclaw_config::integration_plugins::{MailPollSpec, bundled_fastmail};

#[async_trait]
pub(crate) trait EventSourceAdapter: Send + Sync {
    async fn fetch(
        &self,
        watch: &EventWatchConfig,
        server: McpServerConfig,
        checkpoint: &Checkpoint,
    ) -> Result<Snapshot>;
}
struct HomeAssistantAdapter;
struct McpMailAdapter<'a> {
    spec: &'a MailPollSpec,
}

pub(super) async fn fetch(
    watch: &EventWatchConfig,
    server: McpServerConfig,
    checkpoint: &Checkpoint,
    mail_spec: Option<&MailPollSpec>,
) -> Result<Snapshot> {
    match watch.source.as_str() {
        "home_assistant" => HomeAssistantAdapter.fetch(watch, server, checkpoint).await,
        "fastmail" => {
            let default = bundled_fastmail()?;
            let adapter = McpMailAdapter {
                spec: mail_spec.unwrap_or(&default.event),
            };
            adapter.fetch(watch, server, checkpoint).await
        }
        _ => anyhow::bail!("unknown event source adapter"),
    }
}
#[async_trait]
impl EventSourceAdapter for HomeAssistantAdapter {
    async fn fetch(
        &self,
        watch: &EventWatchConfig,
        server: McpServerConfig,
        _checkpoint: &Checkpoint,
    ) -> Result<Snapshot> {
        let mut url = url::Url::parse(server.url.as_deref().context("MCP URL missing")?)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none(),
            "invalid Home Assistant URL"
        );
        let token = server
            .headers
            .get("Authorization")
            .context("Home Assistant Authorization header missing")?;
        // Do not forward account credentials across redirects.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        let mut states = Vec::new();
        for entity in &watch.entities {
            url.set_path(&format!("/api/states/{entity}"));
            let mut response = client
                .get(url.clone())
                .header("Authorization", token)
                .send()
                .await?
                .error_for_status()?;
            ensure!(
                response.status().is_success(),
                "Home Assistant redirects are not accepted"
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    bytes.len() + chunk.len() <= MAX_BODY,
                    "Home Assistant response too large"
                );
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes)?;
            ensure!(
                value["entity_id"].as_str() == Some(entity),
                "unexpected Home Assistant entity"
            );
            let state = value["state"].as_str().context("entity has no state")?;
            ensure!(state.len() <= 128, "entity state too large");
            states.push((entity.clone(), state.to_string()));
        }
        Ok(Snapshot::Home(states))
    }
}
#[async_trait]
impl EventSourceAdapter for McpMailAdapter<'_> {
    async fn fetch(
        &self,
        watch: &EventWatchConfig,
        server: McpServerConfig,
        checkpoint: &Checkpoint,
    ) -> Result<Snapshot> {
        self.spec.validate()?;
        // A fresh short-lived connection each poll resolves rotated credentials and
        // endpoint changes from canonical config, rather than caching live policy.
        let client = McpServer::connect(server).await?;
        let mut mails = Vec::new();
        let mut cursor: Option<String> = None;
        let since = checkpoint.mail_high_water.map(|v| v - MAIL_OVERLAP_MS);
        for page in 0..20 {
            let mut args = json!({});
            args[&self.spec.query_argument] = json!(watch.query);
            args[&self.spec.limit_argument] = json!(50);
            if let Some(cursor) = &cursor {
                args[&self.spec.cursor_argument] = Value::String(cursor.clone());
            }
            let raw = client.call_tool(&self.spec.tool, args).await?;
            let result = tool_data(raw)?;
            let batch = result
                .pointer(&self.spec.items_pointer)
                .context("mail items path missing")?
                .as_array()
                .context("Mail MCP search response has no results")?;
            let mut older = !batch.is_empty();
            for mail in batch {
                let received = mail
                    .pointer(&self.spec.time_pointer)
                    .context("mail timestamp path missing")?
                    .as_str()
                    .context("mail has no receivedAt")?;
                let timestamp = DateTime::parse_from_rfc3339(received)?.timestamp_millis();
                older &= since.is_some_and(|cutoff| timestamp < cutoff);
                let id = mail
                    .pointer(&self.spec.id_pointer)
                    .context("mail ID path missing")?
                    .as_str()
                    .context("mail has no ID")?;
                ensure!(!id.is_empty() && id.len() <= 256, "invalid mail ID");
                // No body, preview, attachments or arbitrary upstream fields enter an event.
                let subject: String = mail
                    .pointer(&self.spec.subject_pointer)
                    .unwrap_or(&Value::Null)
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(512)
                    .collect();
                let from: Vec<String> = mail
                    .pointer(&self.spec.senders_pointer)
                    .unwrap_or(&Value::Null)
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(5)
                    .filter_map(|sender| {
                        sender
                            .pointer(&self.spec.sender_address_pointer)
                            .and_then(Value::as_str)
                    })
                    .map(|email| email.chars().take(254).collect())
                    .collect();
                mails.push(
                json!({"id": id, "received_at_ms": timestamp, "subject": subject, "from": from}),
            );
            }
            if checkpoint.mail_boundary.is_none()
                || older
                || result
                    .pointer(&self.spec.more_pointer)
                    .unwrap_or(&Value::Null)
                    .as_bool()
                    == Some(false)
            {
                return Ok(Snapshot::Mail(mails));
            }
            ensure!(
                result
                    .pointer(&self.spec.more_pointer)
                    .unwrap_or(&Value::Null)
                    .as_bool()
                    == Some(true),
                "Mail MCP pagination state missing"
            );
            let next = result
                .pointer(&self.spec.cursor_pointer)
                .unwrap_or(&Value::Null)
                .as_str()
                .context("Mail MCP pagination cursor missing")?
                .to_string();
            ensure!(
                Some(&next) != cursor.as_ref(),
                "Mail MCP cursor did not advance"
            );
            cursor = Some(next);
            ensure!(
                page < 19,
                "Mail MCP overlap exceeds 1000 messages; checkpoint not advanced"
            );
        }
        anyhow::bail!("Mail MCP pagination did not complete")
    }
}

fn tool_data(raw: Value) -> Result<Value> {
    ensure!(
        raw.get("isError").and_then(Value::as_bool) != Some(true),
        "Mail query failed"
    );
    if let Some(value) = raw.get("structuredContent").filter(|v| v.is_object()) {
        return Ok(value.clone());
    }
    let text = raw["content"]
        .as_array()
        .and_then(|items| items.iter().find_map(|item| item["text"].as_str()))
        .context("Mail query has no structured content")?;
    ensure!(text.len() <= MAX_BODY, "Mail result too large");
    Ok(serde_json::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_text_and_structured_results_without_treating_errors_as_empty() {
        assert_eq!(
            tool_data(json!({"structuredContent":{"results":[],"hasMore":false}})).unwrap()["results"],
            json!([])
        );
        assert!(tool_data(json!({"isError":true,"content":[]})).is_err());
        assert!(tool_data(json!({"content":[{"text":"not JSON"}]})).is_err());
    }
}
