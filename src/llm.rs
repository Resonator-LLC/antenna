///! LLM backend abstraction for SemanticRouter.
///!
///! Trait + implementations:
///!   - OllamaBackend:   POST /api/generate (local Ollama)
///!   - HttpBackend:     POST /v1/chat/completions (OpenAI-compatible)
///!   - PlatformBackend: channel bridge to host app (iOS/Android on-device LLM)

use anyhow::{anyhow, Result};
use std::time::Duration;

use crate::channel::{ChannelReader, ChannelWriter};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

pub trait LlmBackend: Send {
    /// Send system + user prompt, get generated text back.
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<String>;
}

// ---------------------------------------------------------------------------
// Ollama — POST /api/generate
// ---------------------------------------------------------------------------

pub struct OllamaBackend {
    endpoint: String, // e.g. "http://localhost:11434"
    model: String,
}

impl OllamaBackend {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }
}

impl LlmBackend for OllamaBackend {
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<String> {
        let url = format!("{}/api/generate", self.endpoint);
        let body = format!(
            r#"{{"model":"{}","system":{},"prompt":{},"stream":false,"options":{{"num_predict":{}}}}}"#,
            self.model,
            json_escape(system),
            json_escape(user),
            max_tokens,
        );

        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .timeout(Duration::from_secs(120))
            .send_bytes(body.as_bytes())?;

        let text = resp.into_string()?;
        extract_json_field(&text, "response")
    }
}

// ---------------------------------------------------------------------------
// Http — POST /v1/chat/completions (OpenAI-compatible)
// ---------------------------------------------------------------------------

pub struct HttpBackend {
    endpoint: String, // e.g. "http://localhost:8080/v1"
    model: String,
}

impl HttpBackend {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }
}

impl LlmBackend for HttpBackend {
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<String> {
        let url = format!("{}/chat/completions", self.endpoint);
        let body = format!(
            "{{\"model\":\"{}\",\"messages\":[{{\"role\":\"system\",\"content\":{}}},{{\"role\":\"user\",\"content\":{}}}],\"max_tokens\":{}}}",
            self.model,
            json_escape(system),
            json_escape(user),
            max_tokens,
        );

        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .timeout(Duration::from_secs(120))
            .send_bytes(body.as_bytes())?;

        let text = resp.into_string()?;
        // Response: {"choices":[{"message":{"content":"..."}}]}
        extract_nested_content(&text)
    }
}

// ---------------------------------------------------------------------------
// Platform — channel bridge to host app (iOS/Android on-device LLM)
// ---------------------------------------------------------------------------

/// Sends an LlmRequest on a designated channel and blocks until the host
/// replies with an LlmResponse on the response channel.
pub struct PlatformBackend {
    model: String,
    request_writer: ChannelWriter,
    response_reader: ChannelReader,
}

impl PlatformBackend {
    pub fn new(model: &str, request_writer: ChannelWriter, response_reader: ChannelReader) -> Self {
        Self {
            model: model.to_string(),
            request_writer,
            response_reader,
        }
    }
}

impl LlmBackend for PlatformBackend {
    fn complete(&self, system: &str, user: &str, max_tokens: u32) -> Result<String> {
        let request_id = simple_id();

        // Emit request as Turtle on the llmRequest channel
        let request_turtle = format!(
            concat!(
                "@prefix antenna: <http://resonator.network/v2/antenna#> .\n",
                "_:req a antenna:LlmRequest ;\n",
                "    antenna:requestId \"{}\" ;\n",
                "    antenna:model \"{}\" ;\n",
                "    antenna:system {} ;\n",
                "    antenna:prompt {} ;\n",
                "    antenna:maxTokens {} .\n",
            ),
            request_id,
            self.model,
            turtle_escape(system),
            turtle_escape(user),
            max_tokens,
        );

        self.request_writer.send(&request_turtle);

        // Block until response arrives (poll with timeout)
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let mut pfd = libc::pollfd {
                fd: self.response_reader.clock_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis() as i32;
            if remaining <= 0 {
                return Err(anyhow!("platform LLM timeout (120s)"));
            }

            unsafe {
                libc::poll(&mut pfd, 1, remaining.min(500));
            }

            self.response_reader.consume_clock();
            while let Some(turtle) = self.response_reader.recv() {
                // Check if this response matches our request_id
                if turtle.contains(&request_id) {
                    return extract_turtle_completion(&turtle);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct BackendChannels {
    pub request_writer: ChannelWriter,
    pub response_reader: ChannelReader,
}

pub fn create_backend(
    backend_type: &str,
    endpoint: &str,
    model: &str,
    platform_channels: Option<BackendChannels>,
) -> Result<Box<dyn LlmBackend>> {
    match backend_type {
        "ollama" => Ok(Box::new(OllamaBackend::new(endpoint, model))),
        "http" => Ok(Box::new(HttpBackend::new(endpoint, model))),
        "platform" => {
            let ch = platform_channels.ok_or_else(|| {
                anyhow!("platform backend requires llmRequest/llmResponse channels")
            })?;
            Ok(Box::new(PlatformBackend::new(
                model,
                ch.request_writer,
                ch.response_reader,
            )))
        }
        other => Err(anyhow!("unknown LLM backend type: {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

pub fn build_prompt(turtle_a: &str, turtle_b: &str, prefixes: &str) -> String {
    format!(
        "{}\n\nGraph A:\n{}\n\nGraph B:\n{}",
        prefixes.trim(),
        turtle_a.trim(),
        turtle_b.trim(),
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string as a JSON string literal (with quotes).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escape a string as a Turtle long literal (triple-quoted).
fn turtle_escape(s: &str) -> String {
    // Use triple-quoted string to avoid issues with embedded quotes/newlines
    let escaped = s.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"");
    format!("\"\"\"{}\"\"\"", escaped)
}

/// Extract a top-level string field from JSON (minimal parser, no serde).
fn extract_json_field(json: &str, field: &str) -> Result<String> {
    let pattern = format!("\"{}\"", field);
    let pos = json
        .find(&pattern)
        .ok_or_else(|| anyhow!("field '{}' not found in response", field))?;
    let after_key = &json[pos + pattern.len()..];
    // Skip : and whitespace
    let after_colon = after_key
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| anyhow!("malformed JSON after field '{}'", field))?
        .trim_start();

    if !after_colon.starts_with('"') {
        return Err(anyhow!("field '{}' is not a string", field));
    }

    parse_json_string(after_colon)
}

/// Extract content from OpenAI-compatible response.
fn extract_nested_content(json: &str) -> Result<String> {
    // Find "content" field value
    extract_json_field(json, "content")
}

/// Parse a JSON string literal starting at the opening quote.
fn parse_json_string(s: &str) -> Result<String> {
    if !s.starts_with('"') {
        return Err(anyhow!("expected '\"'"));
    }
    let mut out = String::new();
    let mut chars = s[1..].chars();
    loop {
        match chars.next() {
            None => return Err(anyhow!("unterminated JSON string")),
            Some('"') => return Ok(out),
            Some('\\') => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(c) = char::from_u32(cp) {
                            out.push(c);
                        }
                    }
                }
                _ => out.push('?'),
            },
            Some(c) => out.push(c),
        }
    }
}

/// Extract the completion text from an LlmResponse Turtle string.
fn extract_turtle_completion(turtle: &str) -> Result<String> {
    // Look for antenna:completion """...""" or antenna:completion "..."
    let marker = "antenna:completion";
    let pos = turtle
        .find(marker)
        .ok_or_else(|| anyhow!("no antenna:completion in response"))?;
    let after = turtle[pos + marker.len()..].trim_start();

    if let Some(rest) = after.strip_prefix("\"\"\"") {
        // Triple-quoted literal
        let end = rest
            .find("\"\"\"")
            .ok_or_else(|| anyhow!("unterminated triple-quoted literal"))?;
        Ok(rest[..end].to_string())
    } else if after.starts_with('"') {
        // Single-quoted literal
        parse_turtle_string(after)
    } else {
        Err(anyhow!("unexpected token after antenna:completion"))
    }
}

/// Parse a Turtle single-quoted string literal.
fn parse_turtle_string(s: &str) -> Result<String> {
    if !s.starts_with('"') {
        return Err(anyhow!("expected '\"'"));
    }
    let mut out = String::new();
    let mut chars = s[1..].chars();
    loop {
        match chars.next() {
            None => return Err(anyhow!("unterminated string")),
            Some('"') => return Ok(out),
            Some('\\') => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                _ => out.push('?'),
            },
            Some(c) => out.push(c),
        }
    }
}

/// Simple monotonic ID for request correlation.
fn simple_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    format!("{:x}-{:x}", t, n)
}
