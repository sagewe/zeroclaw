use anyhow::{Context, Result, ensure};
use chrono::DateTime;
use serde_json::{Value, json};
use zeroclaw_config::event_watch::EventWatchConfig;
use zeroclaw_config::schema::McpServerConfig;
use zeroclaw_tools::mcp_client::McpServer;

use super::{Checkpoint, Snapshot};

const MAX_BODY: usize = 1024 * 1024;
pub(super) const MAIL_OVERLAP_MS: i64 = 300_000;

pub(super) async fn fetch(
    watch: &EventWatchConfig,
    server: McpServerConfig,
    checkpoint: &Checkpoint,
) -> Result<Snapshot> {
    if watch.source == "home_assistant" {
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
        return Ok(Snapshot::Home(states));
    }

    // A fresh short-lived connection each poll resolves rotated credentials and
    // endpoint changes from canonical config, rather than caching live policy.
    let client = McpServer::connect(server).await?;
    let mut mails = Vec::new();
    let mut cursor: Option<String> = None;
    let since = checkpoint.mail_high_water.map(|v| v - MAIL_OVERLAP_MS);
    for page in 0..20 {
        let mut args = json!({"query": watch.query, "limit": 50});
        if let Some(cursor) = &cursor {
            args["cursor"] = Value::String(cursor.clone());
        }
        let raw = client.call_tool("search_email", args).await?;
        let result = tool_data(raw)?;
        let batch = result["results"]
            .as_array()
            .context("Fastmail search response has no results")?;
        let mut older = !batch.is_empty();
        for mail in batch {
            let received = mail["receivedAt"]
                .as_str()
                .context("mail has no receivedAt")?;
            let timestamp = DateTime::parse_from_rfc3339(received)?.timestamp_millis();
            older &= since.is_some_and(|cutoff| timestamp < cutoff);
            let id = mail["id"].as_str().context("mail has no ID")?;
            ensure!(!id.is_empty() && id.len() <= 256, "invalid mail ID");
            // No body, preview, attachments or arbitrary upstream fields enter an event.
            let subject: String = mail["subject"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(512)
                .collect();
            let from: Vec<String> = mail["from"]
                .as_array()
                .into_iter()
                .flatten()
                .take(5)
                .filter_map(|sender| sender["email"].as_str())
                .map(|email| email.chars().take(254).collect())
                .collect();
            mails.push(
                json!({"id": id, "received_at_ms": timestamp, "subject": subject, "from": from}),
            );
        }
        if checkpoint.mail_boundary.is_none() || older || result["hasMore"].as_bool() == Some(false)
        {
            return Ok(Snapshot::Mail(mails));
        }
        ensure!(
            result["hasMore"].as_bool() == Some(true),
            "Fastmail pagination state missing"
        );
        let next = result["nextCursor"]
            .as_str()
            .context("Fastmail pagination cursor missing")?
            .to_string();
        ensure!(
            Some(&next) != cursor.as_ref(),
            "Fastmail cursor did not advance"
        );
        cursor = Some(next);
        ensure!(
            page < 19,
            "Fastmail overlap exceeds 1000 messages; checkpoint not advanced"
        );
    }
    anyhow::bail!("Fastmail pagination did not complete")
}

fn tool_data(raw: Value) -> Result<Value> {
    ensure!(
        raw.get("isError").and_then(Value::as_bool) != Some(true),
        "Fastmail query failed"
    );
    if let Some(value) = raw
        .get("structuredContent")
        .filter(|v| v.get("results").is_some())
    {
        return Ok(value.clone());
    }
    let text = raw["content"]
        .as_array()
        .and_then(|items| items.iter().find_map(|item| item["text"].as_str()))
        .context("Fastmail query has no structured content")?;
    ensure!(text.len() <= MAX_BODY, "Fastmail result too large");
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
