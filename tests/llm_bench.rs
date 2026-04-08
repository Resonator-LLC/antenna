///! Benchmark: SemanticRouter LLM backend with local Ollama (qwen2.5-coder:7b).
///!
///! Measures:
///!   - Single request latency
///!   - Token generation rate (from Ollama response metadata)
///!   - Throughput (sequential requests/sec)
///!   - Turtle validity rate
///!
///! Requires: Ollama running at localhost:11434 with the model pulled.
///! Run:  cargo test --release --test llm_bench -- --nocapture --ignored
use std::time::{Duration, Instant};

const OLLAMA_ENDPOINT: &str = "http://localhost:11434";
const MODEL: &str = "qwen2.5-coder:7b";
const MAX_TOKENS: u32 = 512;

const SYSTEM_PROMPT: &str = "\
You are an RDF synthesis engine. Given two Turtle RDF graphs, \
produce a third that captures the semantic relationship between them. \
Output ONLY valid Turtle triples. Do not redeclare prefixes. \
Use the same prefix names (res:, rdfs:, xsd:) that appear in the input. No explanation.";

const TURTLE_PREFIXES: &str = "\
@prefix res: <https://resonator.network/> .\n\
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n";

fn user_prompt() -> String {
    format!(
        "{}\n\nGraph A:\n{}\n\nGraph B:\n{}",
        TURTLE_PREFIXES.trim(),
        r#"<urn:user:alice> a res:Person ;
    res:name "Alice" ;
    res:interest "distributed systems" ;
    res:interest "peer-to-peer networks" ;
    res:skill "Rust programming" ;
    res:skill "protocol design" ."#,
        r#"<urn:post:42> a res:Post ;
    rdfs:label "Building a DHT in Rust" ;
    res:topic "distributed hash tables" ;
    res:language "Rust" ;
    res:difficulty "intermediate" ."#,
    )
}

// ---------------------------------------------------------------------------
// Raw Ollama API call with metadata
// ---------------------------------------------------------------------------

struct OllamaResponse {
    response: String,
    eval_count: u64,
    prompt_eval_count: u64,
    eval_duration_ms: f64,
    prompt_eval_duration_ms: f64,
    total_duration_ms: f64,
    done_reason: String,
}

fn ollama_generate(system: &str, prompt: &str, max_tokens: u32) -> Result<OllamaResponse, String> {
    let url = format!("{}/api/generate", OLLAMA_ENDPOINT);
    let body = format!(
        "{{\"model\":\"{}\",\"system\":{},\"prompt\":{},\"stream\":false,\"options\":{{\"num_predict\":{}}}}}",
        MODEL,
        json_escape(system),
        json_escape(prompt),
        max_tokens,
    );

    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(300))
        .send_bytes(body.as_bytes())
        .map_err(|e| format!("HTTP error: {}", e))?;

    let text = resp
        .into_string()
        .map_err(|e| format!("read error: {}", e))?;

    Ok(OllamaResponse {
        response: extract_json_string(&text, "response").unwrap_or_default(),
        eval_count: extract_json_number(&text, "eval_count"),
        prompt_eval_count: extract_json_number(&text, "prompt_eval_count"),
        eval_duration_ms: extract_json_number(&text, "eval_duration") as f64 / 1e6,
        prompt_eval_duration_ms: extract_json_number(&text, "prompt_eval_duration") as f64 / 1e6,
        total_duration_ms: extract_json_number(&text, "total_duration") as f64 / 1e6,
        done_reason: extract_json_string(&text, "done_reason").unwrap_or_default(),
    })
}

fn check_ollama() -> bool {
    match ureq::get(&format!("{}/api/tags", OLLAMA_ENDPOINT))
        .timeout(Duration::from_secs(5))
        .call()
    {
        Ok(resp) => {
            let body = resp.into_string().unwrap_or_default();
            body.contains(MODEL)
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn llm_bench_latency() {
    if !check_ollama() {
        eprintln!("SKIP: Ollama not running or model {} not available", MODEL);
        return;
    }

    let prompt = user_prompt();

    // Warmup (loads model into GPU)
    eprintln!("--- Warmup (loading model) ---");
    let t0 = Instant::now();
    match ollama_generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS) {
        Ok(r) => eprintln!(
            "Warmup: {:.0}ms wall, {} chars, reason: {}\n{}",
            t0.elapsed().as_millis(),
            r.response.len(),
            r.done_reason,
            &r.response[..r.response.len().min(300)],
        ),
        Err(e) => {
            eprintln!("Warmup failed: {}", e);
            return;
        }
    }

    // Latency: 5 samples
    eprintln!("\n--- Latency (5 samples) ---");
    eprintln!(
        "{:>4} {:>8} {:>8} {:>10} {:>10} {:>8} {:>6}",
        "#", "wall_ms", "eval_ms", "tok/s_gen", "tok/s_ppt", "tokens", "chars"
    );

    let mut latencies = Vec::new();
    let mut tok_rates = Vec::new();

    for i in 0..5 {
        let t = Instant::now();
        match ollama_generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS) {
            Ok(r) => {
                let wall_ms = t.elapsed().as_millis();
                let gen_tok_per_sec = if r.eval_duration_ms > 0.0 {
                    r.eval_count as f64 / (r.eval_duration_ms / 1000.0)
                } else {
                    0.0
                };
                let prompt_tok_per_sec = if r.prompt_eval_duration_ms > 0.0 {
                    r.prompt_eval_count as f64 / (r.prompt_eval_duration_ms / 1000.0)
                } else {
                    0.0
                };
                eprintln!(
                    "{:>4} {:>8} {:>8.0} {:>10.1} {:>10.1} {:>8} {:>6}",
                    i + 1,
                    wall_ms,
                    r.eval_duration_ms,
                    gen_tok_per_sec,
                    prompt_tok_per_sec,
                    r.eval_count,
                    r.response.len(),
                );
                latencies.push(wall_ms);
                tok_rates.push(gen_tok_per_sec);
            }
            Err(e) => eprintln!("{:>4} ERROR: {}", i + 1, e),
        }
    }

    if latencies.is_empty() {
        return;
    }

    let avg_ms = latencies.iter().sum::<u128>() / latencies.len() as u128;
    let min_ms = *latencies.iter().min().unwrap();
    let max_ms = *latencies.iter().max().unwrap();
    let avg_tok = tok_rates.iter().sum::<f64>() / tok_rates.len() as f64;

    eprintln!("\n--- Summary ---");
    eprintln!(
        "  Latency avg/min/max: {}ms / {}ms / {}ms",
        avg_ms, min_ms, max_ms
    );
    eprintln!("  Gen tokens/sec avg:  {:.1}", avg_tok);
}

#[test]
#[ignore]
fn llm_bench_throughput() {
    if !check_ollama() {
        eprintln!("SKIP: Ollama not running or model {} not available", MODEL);
        return;
    }

    let prompt = user_prompt();

    // Warmup
    let _ = ollama_generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS);

    let n = 5;
    eprintln!("\n--- Sequential throughput ({} requests) ---", n);

    let mut ok = 0u32;
    let mut total_chars = 0usize;
    let mut total_gen_tokens = 0u64;

    let t_start = Instant::now();
    for i in 0..n {
        let t = Instant::now();
        match ollama_generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS) {
            Ok(r) => {
                total_chars += r.response.len();
                total_gen_tokens += r.eval_count;
                ok += 1;
                eprintln!(
                    "  [{}] {:.0}ms, {} chars, {} tokens",
                    i + 1,
                    t.elapsed().as_millis(),
                    r.response.len(),
                    r.eval_count
                );
            }
            Err(e) => eprintln!("  [{}] FAIL: {}", i + 1, e),
        }
    }
    let total = t_start.elapsed();

    eprintln!("\n--- Throughput ---");
    eprintln!("  Wall time:       {:.1}s", total.as_secs_f64());
    eprintln!("  OK / Total:      {} / {}", ok, n);
    eprintln!("  Requests/sec:    {:.3}", ok as f64 / total.as_secs_f64());
    eprintln!(
        "  Chars/sec:       {:.0}",
        total_chars as f64 / total.as_secs_f64()
    );
    eprintln!(
        "  Avg tokens/req:  {}",
        if ok > 0 {
            total_gen_tokens / ok as u64
        } else {
            0
        }
    );
}

#[test]
#[ignore]
fn llm_bench_turtle_validity() {
    if !check_ollama() {
        eprintln!("SKIP: Ollama not running or model {} not available", MODEL);
        return;
    }

    let prompt = user_prompt();

    // Warmup
    let _ = ollama_generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS);

    let n = 5;
    eprintln!("\n--- Turtle validity ({} samples) ---", n);

    let mut valid = 0u32;
    let mut invalid = 0u32;
    let mut triple_counts = Vec::new();

    for i in 0..n {
        match ollama_generate(SYSTEM_PROMPT, &prompt, MAX_TOKENS) {
            Ok(r) => {
                if r.response.is_empty() {
                    invalid += 1;
                    eprintln!("  [{}] EMPTY (reason: {})", i + 1, r.done_reason);
                    continue;
                }
                let full = format!("{}\n{}", TURTLE_PREFIXES, r.response.trim());
                match parse_turtle(&full) {
                    Ok(count) => {
                        valid += 1;
                        triple_counts.push(count);
                        eprintln!(
                            "  [{}] VALID - {} triples, {} chars",
                            i + 1,
                            count,
                            r.response.len()
                        );
                        eprintln!("       {}", r.response.lines().next().unwrap_or(""));
                    }
                    Err(e) => {
                        invalid += 1;
                        eprintln!("  [{}] INVALID - {}", i + 1, e);
                        eprintln!(
                            "       {}",
                            r.response.chars().take(200).collect::<String>()
                        );
                    }
                }
            }
            Err(e) => {
                invalid += 1;
                eprintln!("  [{}] ERROR: {}", i + 1, e);
            }
        }
    }

    let rate = valid as f64 / n as f64 * 100.0;
    let avg_triples = if !triple_counts.is_empty() {
        triple_counts.iter().sum::<usize>() / triple_counts.len()
    } else {
        0
    };

    eprintln!("\n--- Validity ---");
    eprintln!("  Valid:       {} / {} ({:.0}%)", valid, n, rate);
    eprintln!("  Avg triples: {}", avg_triples);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn extract_json_string(json: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\"", field);
    let pos = json.find(&pattern)?;
    let after = json[pos + pattern.len()..].trim_start();
    let after = after.strip_prefix(':')?;
    let after = after.trim_start();
    if !after.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = after[1..].chars();
    loop {
        match chars.next() {
            None | Some('"') => return Some(out),
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
                _ => {}
            },
            Some(c) => out.push(c),
        }
    }
}

fn extract_json_number(json: &str, field: &str) -> u64 {
    let pattern = format!("\"{}\"", field);
    let Some(pos) = json.find(&pattern) else {
        return 0;
    };
    let after = json[pos + pattern.len()..].trim_start();
    let Some(after) = after.strip_prefix(':') else {
        return 0;
    };
    let after = after.trim_start();
    let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    num_str.parse().unwrap_or(0)
}

fn parse_turtle(turtle: &str) -> Result<usize, String> {
    use oxigraph::io::{RdfFormat, RdfParser};
    let parser = RdfParser::from_format(RdfFormat::Turtle);
    let mut count = 0;
    for result in parser.for_reader(turtle.as_bytes()) {
        match result {
            Ok(_) => count += 1,
            Err(e) => return Err(format!("{}", e)),
        }
    }
    if count == 0 {
        return Err("no triples".to_string());
    }
    Ok(count)
}
