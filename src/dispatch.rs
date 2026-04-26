// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Reactive router: parse incoming Turtle, dispatch by rdf:type.

use crate::carrier::CarrierClient;
use crate::channel::AntennaOut;
use crate::dag::Dag;
use crate::store::RdfStore;

const SP_NS: &str = "http://spinrdf.org/sp#";
const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const CARRIER_NS: &str = "http://resonator.network/v2/carrier#";

/// Dispatch a single Turtle line based on its rdf:type.
///
/// `carrier` is optional so the same router works from contexts without a
/// carrier handle (integration tests, tools). `default_account` is used when
/// a carrier command omits `carrier:account` — the antenna bootstrap
/// account_id is the natural fallback.
pub fn dispatch(
    line: &str,
    store: &RdfStore,
    dag: &Dag,
    carrier: Option<&CarrierClient>,
    default_account: &str,
    out: &mut dyn AntennaOut,
) {
    if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
        return;
    }

    let rdf_type_opt = extract_type(line);
    tracing::debug!(
        target: "DISPATCH",
        rdf_type = rdf_type_opt.as_deref().unwrap_or("<none>"),
        bytes = line.len(),
        "route",
    );
    let rdf_type = match rdf_type_opt {
        Some(t) => t,
        None => {
            insert_with_dag(line, store, dag, out);
            return;
        }
    };

    if rdf_type.starts_with(SP_NS) {
        handle_spin(line, &rdf_type, store, out);
    } else if rdf_type.starts_with(CARRIER_NS) {
        match carrier {
            Some(c) => handle_carrier(line, &rdf_type, c, default_account, out),
            None => tracing::warn!(target: "DISPATCH", %rdf_type, "carrier dispatch skipped (no handle)"),
        }
    } else {
        insert_with_dag(line, store, dag, out);
    }
}

// ---------------------------------------------------------------------------
// SPIN query handling
// ---------------------------------------------------------------------------

fn handle_spin(line: &str, rdf_type: &str, store: &RdfStore, out: &mut dyn AntennaOut) {
    let local = &rdf_type[SP_NS.len()..];
    let sp_text = extract_property(line, "sp:text")
        .or_else(|| extract_property(line, &format!("<{}text>", SP_NS)));

    let sparql = match sp_text {
        Some(s) => s,
        None => {
            out.send(&format!(
                "[] a antenna:Error ; antenna:message \"Missing sp:text in {}\" .",
                local
            ));
            return;
        }
    };

    match local {
        "Select" => {
            let start = std::time::Instant::now();
            match store.query(&sparql) {
                Ok(results) => {
                    use oxigraph::sparql::QueryResults;
                    let mut rows = 0u64;
                    if let QueryResults::Solutions(solutions) = results {
                        for sol in solutions.flatten() {
                            rows += 1;
                            let mut parts = Vec::new();
                            for (var, term) in sol.iter() {
                                parts.push(format!(
                                    "antenna:var_{} \"{}\"",
                                    var.as_str(),
                                    turtle_escape(&term.to_string())
                                ));
                            }
                            if !parts.is_empty() {
                                out.send(&format!(
                                    "[] a antenna:Result ; {} .",
                                    parts.join(" ; ")
                                ));
                            }
                        }
                    }
                    tracing::debug!(
                        target: "SPARQL",
                        op = "select",
                        rows,
                        ms = start.elapsed().as_millis() as u64,
                    );
                }
                Err(e) => {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        "Ask" => {
            let start = std::time::Instant::now();
            match store.ask(&sparql) {
                Ok(b) => {
                    tracing::debug!(
                        target: "SPARQL",
                        op = "ask",
                        ok = b,
                        ms = start.elapsed().as_millis() as u64,
                    );
                    out.send(&format!("[] a sp:AskResult ; sp:boolean {} .", b));
                }
                Err(e) => {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        "Construct" => {
            let start = std::time::Instant::now();
            match store.query(&sparql) {
                Ok(results) => {
                    use oxigraph::sparql::QueryResults;
                    let mut rows = 0u64;
                    if let QueryResults::Graph(triples) = results {
                        for t in triples.flatten() {
                            rows += 1;
                            let turtle = format!("{} {} {} .", t.subject, t.predicate, t.object);
                            out.send(&turtle);
                            let _ = store.insert_turtle(&turtle);
                        }
                    }
                    tracing::debug!(
                        target: "SPARQL",
                        op = "construct",
                        rows,
                        ms = start.elapsed().as_millis() as u64,
                    );
                }
                Err(e) => {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        "InsertData" | "DeleteData" | "Modify" => {
            let start = std::time::Instant::now();
            match store.update(&sparql) {
                Ok(()) => {
                    tracing::debug!(
                        target: "SPARQL",
                        op = "update",
                        ms = start.elapsed().as_millis() as u64,
                    );
                }
                Err(e) => {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        _ => {
            out.send(&format!(
                "[] a antenna:Error ; antenna:message \"Unknown SPIN type: {}\" .",
                local
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Carrier (v0.2) command handling
// ---------------------------------------------------------------------------

fn account_or_default(line: &str, default_account: &str) -> String {
    extract_property(line, "carrier:account")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_account.to_string())
}

fn carrier_error(out: &mut dyn AntennaOut, command: &str, err: &dyn std::fmt::Display) {
    out.send(&format!(
        "[] a antenna:Error ; antenna:message \"{} failed: {}\" .",
        command,
        turtle_escape(&err.to_string())
    ));
}

fn missing_field(out: &mut dyn AntennaOut, command: &str, field: &str) {
    out.send(&format!(
        "[] a antenna:Error ; antenna:message \"{} missing {}\" .",
        command, field
    ));
}

fn handle_carrier(
    line: &str,
    rdf_type: &str,
    carrier: &CarrierClient,
    default_account: &str,
    out: &mut dyn AntennaOut,
) {
    let local = &rdf_type[CARRIER_NS.len()..];
    match local {
        "CreateAccount" => {
            let display_name = extract_property(line, "carrier:displayName");
            match carrier.create_account(display_name.as_deref()) {
                Ok(_) => {}
                Err(e) => carrier_error(out, "CreateAccount", &e),
            }
        }
        "LoadAccount" => {
            let account = match extract_property(line, "carrier:account") {
                Some(a) if !a.is_empty() => a,
                _ => {
                    missing_field(out, "LoadAccount", "carrier:account");
                    return;
                }
            };
            if let Err(e) = carrier.load_account(&account) {
                carrier_error(out, "LoadAccount", &e);
            }
        }
        "GetId" => {
            let account = account_or_default(line, default_account);
            if let Err(e) = carrier.get_id(&account) {
                carrier_error(out, "GetId", &e);
            }
        }
        "SetNick" => {
            let account = account_or_default(line, default_account);
            let nick = extract_property(line, "carrier:displayName")
                .or_else(|| extract_property(line, "carrier:nick"));
            let nick = match nick {
                Some(n) => n,
                None => {
                    missing_field(out, "SetNick", "carrier:displayName");
                    return;
                }
            };
            if let Err(e) = carrier.set_nick(&account, &nick) {
                carrier_error(out, "SetNick", &e);
            }
        }
        "SendTrustRequest" => {
            let account = account_or_default(line, default_account);
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, "SendTrustRequest", "carrier:contactUri");
                    return;
                }
            };
            let payload = extract_property(line, "carrier:payload")
                .or_else(|| extract_property(line, "carrier:message"));
            if let Err(e) = carrier.send_trust_request(&account, &uri, payload.as_deref()) {
                carrier_error(out, "SendTrustRequest", &e);
            }
        }
        "AcceptTrustRequest" => {
            let account = account_or_default(line, default_account);
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, "AcceptTrustRequest", "carrier:contactUri");
                    return;
                }
            };
            if let Err(e) = carrier.accept_trust_request(&account, &uri) {
                carrier_error(out, "AcceptTrustRequest", &e);
            }
        }
        "DiscardTrustRequest" => {
            let account = account_or_default(line, default_account);
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, "DiscardTrustRequest", "carrier:contactUri");
                    return;
                }
            };
            if let Err(e) = carrier.discard_trust_request(&account, &uri) {
                carrier_error(out, "DiscardTrustRequest", &e);
            }
        }
        "RemoveContact" => {
            let account = account_or_default(line, default_account);
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, "RemoveContact", "carrier:contactUri");
                    return;
                }
            };
            if let Err(e) = carrier.remove_contact(&account, &uri) {
                carrier_error(out, "RemoveContact", &e);
            }
        }
        "SendMsg" => {
            let account = account_or_default(line, default_account);
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, "SendMsg", "carrier:contactUri");
                    return;
                }
            };
            let text = match extract_property(line, "carrier:text") {
                Some(t) => t,
                None => {
                    missing_field(out, "SendMsg", "carrier:text");
                    return;
                }
            };
            if let Err(e) = carrier.send_message(&account, &uri, &text) {
                carrier_error(out, "SendMsg", &e);
            }
        }
        // CreateGroup is the v0.2 vocabulary name; CreateConversation is an
        // alias since both libjami and the C API use the latter spelling.
        "CreateConversation" | "CreateGroup" => {
            let account = account_or_default(line, default_account);
            let privacy = extract_property(line, "carrier:privacy");
            if let Err(e) = carrier.create_conversation(&account, privacy.as_deref()) {
                carrier_error(out, local, &e);
            }
        }
        "SendConversationMsg" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "SendConversationMsg", "carrier:conversationId");
                    return;
                }
            };
            let text = match extract_property(line, "carrier:text") {
                Some(t) => t,
                None => {
                    missing_field(out, "SendConversationMsg", "carrier:text");
                    return;
                }
            };
            if let Err(e) = carrier.send_conversation_message(&account, &conv, &text) {
                carrier_error(out, "SendConversationMsg", &e);
            }
        }
        "AcceptConversationRequest" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "AcceptConversationRequest", "carrier:conversationId");
                    return;
                }
            };
            if let Err(e) = carrier.accept_conversation_request(&account, &conv) {
                carrier_error(out, "AcceptConversationRequest", &e);
            }
        }
        "DeclineConversationRequest" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "DeclineConversationRequest", "carrier:conversationId");
                    return;
                }
            };
            if let Err(e) = carrier.decline_conversation_request(&account, &conv) {
                carrier_error(out, "DeclineConversationRequest", &e);
            }
        }
        "InviteContact" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "InviteContact", "carrier:conversationId");
                    return;
                }
            };
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, "InviteContact", "carrier:contactUri");
                    return;
                }
            };
            if let Err(e) = carrier.invite_to_conversation(&account, &conv, &uri) {
                carrier_error(out, "InviteContact", &e);
            }
        }
        "RemoveConversation" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "RemoveConversation", "carrier:conversationId");
                    return;
                }
            };
            if let Err(e) = carrier.remove_conversation(&account, &conv) {
                carrier_error(out, "RemoveConversation", &e);
            }
        }
        "SendReaction" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "SendReaction", "carrier:conversationId");
                    return;
                }
            };
            let msg = match extract_property(line, "carrier:messageId") {
                Some(m) => m,
                None => {
                    missing_field(out, "SendReaction", "carrier:messageId");
                    return;
                }
            };
            let react = match extract_property(line, "carrier:reaction") {
                Some(r) => r,
                None => {
                    missing_field(out, "SendReaction", "carrier:reaction");
                    return;
                }
            };
            if let Err(e) = carrier.send_reaction(&account, &conv, &msg, &react) {
                carrier_error(out, "SendReaction", &e);
            }
        }
        cmd @ ("SubscribePresence" | "UnsubscribePresence") => {
            let account = account_or_default(line, default_account);
            let uri = match extract_property(line, "carrier:contactUri") {
                Some(u) => u,
                None => {
                    missing_field(out, cmd, "carrier:contactUri");
                    return;
                }
            };
            let subscribe = cmd == "SubscribePresence";
            if let Err(e) = carrier.subscribe_presence(&account, &uri, subscribe) {
                carrier_error(out, cmd, &e);
            }
        }
        "SendFile" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "SendFile", "carrier:conversationId");
                    return;
                }
            };
            let path = match extract_property(line, "carrier:path") {
                Some(p) => p,
                None => {
                    missing_field(out, "SendFile", "carrier:path");
                    return;
                }
            };
            let display = extract_property(line, "carrier:filename")
                .or_else(|| extract_property(line, "carrier:displayName"));
            if let Err(e) = carrier.send_file(&account, &conv, &path, display.as_deref()) {
                carrier_error(out, "SendFile", &e);
            }
        }
        "AcceptFile" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "AcceptFile", "carrier:conversationId");
                    return;
                }
            };
            let msg = match extract_property(line, "carrier:messageId") {
                Some(m) => m,
                None => {
                    missing_field(out, "AcceptFile", "carrier:messageId");
                    return;
                }
            };
            let fid = match extract_property(line, "carrier:fileId") {
                Some(f) => f,
                None => {
                    missing_field(out, "AcceptFile", "carrier:fileId");
                    return;
                }
            };
            let path = match extract_property(line, "carrier:path") {
                Some(p) => p,
                None => {
                    missing_field(out, "AcceptFile", "carrier:path");
                    return;
                }
            };
            if let Err(e) = carrier.accept_file(&account, &conv, &msg, &fid, &path) {
                carrier_error(out, "AcceptFile", &e);
            }
        }
        "CancelFile" => {
            let account = account_or_default(line, default_account);
            let conv = match extract_property(line, "carrier:conversationId") {
                Some(c) => c,
                None => {
                    missing_field(out, "CancelFile", "carrier:conversationId");
                    return;
                }
            };
            let fid = match extract_property(line, "carrier:fileId") {
                Some(f) => f,
                None => {
                    missing_field(out, "CancelFile", "carrier:fileId");
                    return;
                }
            };
            if let Err(e) = carrier.cancel_file(&account, &conv, &fid) {
                carrier_error(out, "CancelFile", &e);
            }
        }
        "LinkDevice" => {
            // No payload — fire-and-forget. The new account_id appears
            // synchronously inside libcarrier; the DeviceLinkPin event
            // surfaces it externally.
            if let Err(e) = carrier.create_linking_account() {
                carrier_error(out, "LinkDevice", &e);
            }
        }
        "AuthorizeDevice" => {
            let account = account_or_default(line, default_account);
            let pin = match extract_property(line, "carrier:pin") {
                Some(p) => p,
                None => {
                    missing_field(out, "AuthorizeDevice", "carrier:pin");
                    return;
                }
            };
            if let Err(e) = carrier.authorize_device(&account, &pin) {
                carrier_error(out, "AuthorizeDevice", &e);
            }
        }
        "RevokeDevice" => {
            let account = account_or_default(line, default_account);
            let device_id = match extract_property(line, "carrier:contactUri") {
                Some(d) => d,
                None => {
                    missing_field(out, "RevokeDevice", "carrier:contactUri");
                    return;
                }
            };
            if let Err(e) = carrier.revoke_device(&account, &device_id) {
                carrier_error(out, "RevokeDevice", &e);
            }
        }
        other => {
            tracing::warn!(target: "DISPATCH", command = %other, "carrier command not implemented");
        }
    }
}

// ---------------------------------------------------------------------------
// Raw RDF insert through the DAG
// ---------------------------------------------------------------------------

fn insert_with_dag(line: &str, store: &RdfStore, dag: &Dag, out: &mut dyn AntennaOut) {
    dag.before_insert(line);

    if let Err(e) = store.insert_turtle(line) {
        tracing::warn!(target: "SPARQL", %e, "insert error");
        return;
    }

    dag.after_insert(line);
    out.send(line);
}

// ---------------------------------------------------------------------------
// Lightweight Turtle parsing
// ---------------------------------------------------------------------------

pub fn extract_type(line: &str) -> Option<String> {
    let line = line.trim();

    if let Some(pos) = line.find(" a ") {
        let after = &line[pos + 3..].trim_start();

        if after.starts_with('<') {
            if let Some(end) = after.find('>') {
                return Some(after[1..end].to_string());
            }
            return None;
        }

        let type_str = after.split([' ', ';', '.']).next()?.trim();

        if let Some((prefix, local)) = type_str.split_once(':') {
            let ns = match prefix {
                "sp" => SP_NS,
                "spin" => "http://spinrdf.org/spin#",
                "carrier" => CARRIER_NS,
                "antenna" => ANTENNA_NS,
                "rdf" => "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
                _ => return None,
            };
            return Some(format!("{}{}", ns, local));
        }
    }

    None
}

pub fn extract_property(line: &str, prop: &str) -> Option<String> {
    let search = format!("{} ", prop);
    if let Some(pos) = line.find(&search) {
        let after = &line[pos + search.len()..];
        let after = after.trim();

        if let Some(inner) = after.strip_prefix('"') {
            let mut end = 0;
            let mut escaped = false;
            for (i, c) in inner.chars().enumerate() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                if c == '"' {
                    end = i;
                    break;
                }
            }
            if end > 0 {
                return Some(inner[..end].to_string());
            }
        } else {
            let val = after.split([' ', ';', '.']).next()?.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }

    None
}

pub fn turtle_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_type_prefixed_sp() {
        let line = r#"[] a sp:Select ; sp:text "SELECT ?s WHERE { ?s ?p ?o }" ."#;
        assert_eq!(
            extract_type(line),
            Some("http://spinrdf.org/sp#Select".to_string())
        );
    }

    #[test]
    fn extract_type_prefixed_carrier() {
        let line = "[] a carrier:GetId .";
        assert_eq!(
            extract_type(line),
            Some("http://resonator.network/v2/carrier#GetId".to_string())
        );
    }

    #[test]
    fn extract_type_full_uri() {
        let line = "[] a <http://example.org/Foo> ; rdfs:label \"bar\" .";
        assert_eq!(
            extract_type(line),
            Some("http://example.org/Foo".to_string())
        );
    }

    #[test]
    fn extract_type_antenna() {
        let line = "[] a antenna:Bookmark ; rdfs:label \"test\" .";
        assert_eq!(
            extract_type(line),
            Some("http://resonator.network/v2/antenna#Bookmark".to_string())
        );
    }

    #[test]
    fn extract_type_none_for_no_type() {
        assert_eq!(extract_type("[] rdfs:label \"foo\" ."), None);
    }

    #[test]
    fn extract_type_none_for_comment() {
        assert_eq!(extract_type("# this is a comment"), None);
    }

    #[test]
    fn extract_type_none_for_unknown_prefix() {
        assert_eq!(extract_type("[] a custom:Foo ."), None);
    }

    #[test]
    fn extract_property_quoted() {
        let line = r#"[] a sp:Select ; sp:text "SELECT ?s WHERE { ?s ?p ?o }" ."#;
        assert_eq!(
            extract_property(line, "sp:text"),
            Some("SELECT ?s WHERE { ?s ?p ?o }".to_string())
        );
    }

    #[test]
    fn extract_property_uri_value() {
        let line = "[] a carrier:SendMsg ; carrier:contactUri \"abc123\" ; carrier:text \"hi\" .";
        assert_eq!(
            extract_property(line, "carrier:contactUri"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn extract_property_escaped_quotes() {
        let line = r#"[] a antenna:Error ; antenna:message "say \"hello\"" ."#;
        assert_eq!(
            extract_property(line, "antenna:message"),
            Some(r#"say \"hello\""#.to_string())
        );
    }

    #[test]
    fn extract_property_missing() {
        let line = "[] a sp:Select .";
        assert_eq!(extract_property(line, "sp:text"), None);
    }

    #[test]
    fn extract_property_boolean() {
        let line = "[] a sp:AskResult ; sp:boolean true .";
        assert_eq!(
            extract_property(line, "sp:boolean"),
            Some("true".to_string())
        );
    }

    #[test]
    fn turtle_escape_basic() {
        assert_eq!(turtle_escape("hello"), "hello");
    }

    #[test]
    fn turtle_escape_quotes_and_newlines() {
        assert_eq!(
            turtle_escape("say \"hi\"\nnewline"),
            "say \\\"hi\\\"\\nnewline"
        );
    }

    #[test]
    fn turtle_escape_backslash() {
        assert_eq!(turtle_escape("a\\b"), "a\\\\b");
    }

    #[test]
    fn turtle_escape_carriage_return() {
        assert_eq!(turtle_escape("a\rb"), "a\\rb");
    }

    struct TestOut {
        messages: std::cell::RefCell<Vec<String>>,
    }

    impl TestOut {
        fn new() -> Self {
            Self {
                messages: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn messages(&self) -> Vec<String> {
            self.messages.borrow().clone()
        }
    }

    impl AntennaOut for TestOut {
        fn send(&mut self, turtle: &str) {
            self.messages.borrow_mut().push(turtle.to_string());
        }
    }

    #[test]
    fn handle_spin_select_with_results() {
        let store = RdfStore::open(None).unwrap();
        store
            .insert_turtle("<urn:x> a <urn:Foo> ; <urn:val> \"hello\" .")
            .unwrap();
        let mut out = TestOut::new();
        let line = r#"[] a sp:Select ; sp:text "SELECT ?s ?v WHERE { ?s <urn:val> ?v }" ."#;
        handle_spin(line, &format!("{}Select", SP_NS), &store, &mut out);
        let msgs = out.messages();
        assert!(!msgs.is_empty(), "should return at least one result");
        assert!(msgs[0].contains("antenna:Result"), "should be a Result type");
        assert!(msgs[0].contains("hello"), "should contain the value");
    }

    #[test]
    fn handle_spin_ask_true() {
        let store = RdfStore::open(None).unwrap();
        store.insert_turtle("<urn:x> a <urn:Foo> .").unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:Ask ; sp:text "ASK { <urn:x> a <urn:Foo> }" ."#,
            &format!("{}Ask", SP_NS),
            &store,
            &mut out,
        );
        let msgs = out.messages();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("true"));
    }

    #[test]
    fn handle_spin_ask_false() {
        let store = RdfStore::open(None).unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:Ask ; sp:text "ASK { <urn:x> a <urn:Nothing> }" ."#,
            &format!("{}Ask", SP_NS),
            &store,
            &mut out,
        );
        let msgs = out.messages();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("false"));
    }

    #[test]
    fn handle_spin_insert_data() {
        let store = RdfStore::open(None).unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:InsertData ; sp:text "INSERT DATA { <urn:y> a <urn:Bar> }" ."#,
            &format!("{}InsertData", SP_NS),
            &store,
            &mut out,
        );
        assert!(out.messages().is_empty(), "no error expected");
        assert!(store.ask("ASK { <urn:y> a <urn:Bar> }").unwrap());
    }

    #[test]
    fn handle_spin_modify_runs_delete_and_insert() {
        let store = RdfStore::open(None).unwrap();
        store
            .insert_turtle("<urn:counter:panel> <urn:counter:count> \"0\" .")
            .unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:Modify ; sp:text "DELETE WHERE { <urn:counter:panel> <urn:counter:count> ?c }" ."#,
            &format!("{}Modify", SP_NS),
            &store,
            &mut out,
        );
        assert!(out.messages().is_empty(), "no error expected");
        assert!(
            !store
                .ask("ASK { <urn:counter:panel> <urn:counter:count> ?c }")
                .unwrap(),
            "DELETE WHERE must have removed the triple"
        );
    }

    #[test]
    fn handle_spin_delete_data() {
        let store = RdfStore::open(None).unwrap();
        store.insert_turtle("<urn:z> a <urn:Baz> .").unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:DeleteData ; sp:text "DELETE DATA { <urn:z> a <urn:Baz> }" ."#,
            &format!("{}DeleteData", SP_NS),
            &store,
            &mut out,
        );
        assert!(out.messages().is_empty());
        assert!(!store.ask("ASK { <urn:z> a <urn:Baz> }").unwrap());
    }

    #[test]
    fn handle_spin_missing_sp_text() {
        let store = RdfStore::open(None).unwrap();
        let mut out = TestOut::new();
        handle_spin(
            "[] a sp:Select .",
            &format!("{}Select", SP_NS),
            &store,
            &mut out,
        );
        let msgs = out.messages();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("Missing sp:text"));
    }

    #[test]
    fn handle_spin_unknown_type() {
        let store = RdfStore::open(None).unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:Bogus ; sp:text "SELECT 1" ."#,
            &format!("{}Bogus", SP_NS),
            &store,
            &mut out,
        );
        let msgs = out.messages();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("Unknown SPIN type"));
    }

    #[test]
    fn handle_spin_sparql_syntax_error() {
        let store = RdfStore::open(None).unwrap();
        let mut out = TestOut::new();
        handle_spin(
            r#"[] a sp:Select ; sp:text "NOT VALID SPARQL !!!" ."#,
            &format!("{}Select", SP_NS),
            &store,
            &mut out,
        );
        let msgs = out.messages();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("antenna:Error"));
    }

    #[test]
    fn insert_with_dag_stores_and_emits() {
        let store = RdfStore::open(None).unwrap();
        let dag = Dag::load(&store).unwrap();
        let mut out = TestOut::new();
        insert_with_dag("<urn:a> a <urn:Thing> .", &store, &dag, &mut out);
        assert!(store.ask("ASK { <urn:a> a <urn:Thing> }").unwrap());
        let msgs = out.messages();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("urn:a"));
    }

    #[test]
    fn insert_with_dag_invalid_turtle() {
        let store = RdfStore::open(None).unwrap();
        let dag = Dag::load(&store).unwrap();
        let mut out = TestOut::new();
        insert_with_dag("this is not valid turtle", &store, &dag, &mut out);
        assert!(out.messages().is_empty());
    }
}
