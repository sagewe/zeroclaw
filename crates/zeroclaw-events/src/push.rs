//! JMAP push is a wake-up hint. MCP still owns mail reads; SQLite owns deduplication.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::time::Duration;
use zeroclaw_config::integration_plugins::JmapPushSpec;

const MAX_FRAME: usize = 64 * 1024;
const MAIL: &str = "urn:ietf:params:jmap:mail";

pub(super) struct PushConnection {
    response: reqwest::Response,
    parser: SseParser,
}
impl PushConnection {
    pub async fn connect(spec: &JmapPushSpec, token: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        let authorization = if token.starts_with("Bearer ") {
            token.to_owned()
        } else {
            format!("Bearer {}", token.trim())
        };
        let mut authorization = reqwest::header::HeaderValue::from_str(&authorization)
            .context("invalid JMAP authorization header")?;
        authorization.set_sensitive(true);
        let mut response = client
            .get(&spec.session_url)
            .header(reqwest::header::AUTHORIZATION, authorization.clone())
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .context("JMAP session connection failed")?;
        ensure!(
            response.status().is_success(),
            "JMAP session rejected (HTTP {})",
            response.status().as_u16()
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= MAX_FRAME,
                "JMAP session too large"
            );
            body.extend_from_slice(&chunk);
        }
        let session: Value = serde_json::from_slice(&body).context("invalid JMAP session")?;
        let account = session["primaryAccounts"][MAIL]
            .as_str()
            .context("JMAP mail account missing")?
            .to_owned();
        ensure!(
            !account.is_empty() && account.len() <= 255,
            "invalid JMAP account id"
        );
        let template = session["eventSourceUrl"]
            .as_str()
            .context("JMAP event source unavailable")?;
        let url = event_url(template, &spec.event_origins)?;
        let response = tokio::time::timeout(
            Duration::from_secs(20),
            client
                .get(url)
                .header(reqwest::header::AUTHORIZATION, authorization.clone())
                .header("Accept", "text/event-stream")
                .send(),
        )
        .await
        .context("JMAP push connection timed out")?
        .context("JMAP push connection failed")?;
        ensure!(
            response.status().is_success(),
            "JMAP push rejected (HTTP {})",
            response.status().as_u16()
        );
        ensure!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(';').next() == Some("text/event-stream")),
            "JMAP push did not return an event stream"
        );
        Ok(Self {
            response,
            parser: SseParser::new(account),
        })
    }
    /// Any chunk proves transport activity; only a changed Email state requests a mail read.
    pub async fn next(&mut self) -> Result<bool> {
        let chunk = self
            .response
            .chunk()
            .await
            .context("JMAP push stream failed")?
            .context("JMAP push stream closed")?;
        self.parser.feed(&chunk)
    }
}

fn event_url(template: &str, allowed: &[String]) -> Result<url::Url> {
    ensure!(template.len() <= 4096, "JMAP event URL too long");
    let expanded = template
        .replace("{types}", "Email")
        .replace("{closeafter}", "no")
        .replace("{ping}", "30");
    ensure!(
        !expanded.contains(['{', '}']),
        "unsupported JMAP event URL template"
    );
    let mut url = url::Url::parse(&expanded).context("invalid JMAP event URL")?;
    ensure!(
        allowed.contains(&url.origin().ascii_serialization())
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "JMAP event origin is not approved by the package"
    );
    // Fastmail also returns bare URLs without the optional template variables.
    let pairs = url
        .query_pairs()
        .filter(|(k, _)| !matches!(k.as_ref(), "types" | "closeafter" | "ping"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("types", "Email")
        .append_pair("closeafter", "no")
        .append_pair("ping", "30");
    Ok(url)
}

struct SseParser {
    account: String,
    pending: Vec<u8>,
    event: String,
    data: String,
    last_state: Option<String>,
}
impl SseParser {
    fn new(account: String) -> Self {
        Self {
            account,
            pending: Vec::new(),
            event: String::new(),
            data: String::new(),
            last_state: None,
        }
    }
    fn feed(&mut self, bytes: &[u8]) -> Result<bool> {
        let mut changed = false;
        for byte in bytes {
            if *byte != b'\n' {
                self.pending.push(*byte);
                ensure!(
                    self.pending.len() + self.data.len() <= MAX_FRAME,
                    "JMAP push frame too large"
                );
                continue;
            }
            let line = String::from_utf8(std::mem::take(&mut self.pending))
                .context("JMAP push is not UTF-8")?;
            let line = line.trim_end_matches('\r').trim_start_matches('\u{feff}');
            if line.is_empty() {
                if !self.data.is_empty() && matches!(self.event.as_str(), "state" | "message" | "")
                {
                    let value: Value =
                        serde_json::from_str(&self.data).context("invalid JMAP state frame")?;
                    // The server may send its legacy connect/change shape. Only the selected
                    // account's Email state is consumed, never content or instructions.
                    if let Some(state) = value["changed"][&self.account]["Email"].as_str() {
                        ensure!(state.len() <= 4096, "JMAP state too long");
                        if self.last_state.as_deref() != Some(state) {
                            self.last_state = Some(state.to_owned());
                            changed = true;
                        }
                    }
                }
                self.event.clear();
                self.data.clear();
            } else if !line.starts_with(':') {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => self.event = value.to_owned(),
                    "data" => {
                        self.data.push_str(value);
                        self.data.push('\n');
                    }
                    _ => {} // IDs are unnecessary: every reconnect performs reconciliation.
                }
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_frames_ping_duplicate_and_other_accounts_do_not_trigger_mail_reads() {
        let mut parser = SseParser::new("a".into());
        assert!(
            !parser
                .feed(b"event: ping\r\ndata: {\"interval\":30}\r\n\r\n")
                .unwrap()
        );
        let frame=b"event: state\ndata: {\"@type\":\"StateChange\",\n data ignored\ndata: \"changed\":{\"a\":{\"Email\":\"one\"}}}\n\n";
        let mut triggers = 0;
        for part in frame.chunks(3) {
            triggers += usize::from(parser.feed(part).unwrap());
        }
        assert_eq!(triggers, 1);
        assert!(
            !parser
                .feed(b"data: {\"changed\":{\"a\":{\"Email\":\"one\"}}}\n\n")
                .unwrap()
        );
        assert!(
            !parser
                .feed(b"data: {\"changed\":{\"other\":{\"Email\":\"two\"}}}\n\n")
                .unwrap()
        );
        assert!(
            parser
                .feed(b"data: {\"changed\":{\"a\":{\"Email\":\"two\"}}}\n\n")
                .unwrap()
        );
    }
    #[test]
    fn invalid_or_unbounded_frames_fail_closed() {
        assert!(
            SseParser::new("a".into())
                .feed(&vec![b'x'; MAX_FRAME + 1])
                .is_err()
        );
        assert!(
            SseParser::new("a".into())
                .feed(b"event: state\ndata: invalid\n\n")
                .is_err()
        );
    }
    #[test]
    fn session_discovery_cannot_forward_credentials_outside_pinned_origins() {
        let allowed = vec!["https://push.example.invalid".into()];
        assert!(event_url("https://attacker.invalid/events", &allowed).is_err());
        assert!(event_url("https://user:pass@push.example.invalid/events", &allowed).is_err());
        let url = event_url(
            "https://push.example.invalid/events?types={types}&ping={ping}&closeafter={closeafter}",
            &allowed,
        )
        .unwrap();
        assert_eq!(url.query_pairs().count(), 3);
        assert!(url.as_str().contains("types=Email"));
    }
}
