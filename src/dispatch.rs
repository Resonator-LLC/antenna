// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Reactive router: parse incoming Turtle, dispatch by rdf:type.

use crate::carrier_tox::ToxCarrier;
use crate::channel::AntennaOut;
use crate::dag::Dag;
use crate::store::RdfStore;

const SP_NS: &str = "http://spinrdf.org/sp#";
const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const TOX_NS: &str = "http://resonator.network/v2/carrier#";

/// Dispatch a single Turtle line based on its rdf:type.
///
/// `tox` is optional so the same router works from contexts that do not
/// own a carrier handle (integration tests, tools). If a `carrier:` type
/// arrives with `tox = None`, the dispatch is skipped with a warning —
/// the turtle is neither stored nor echoed.
pub fn dispatch(
    line: &str,
    store: &RdfStore,
    dag: &Dag,
    tox: Option<&ToxCarrier>,
    out: &mut dyn AntennaOut,
) {
    if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
        return;
    }

    // Try to extract the rdf:type from the Turtle statement
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
            // No recognizable type — treat as raw RDF, insert into store
            insert_with_dag(line, store, dag, out);
            return;
        }
    };

    // Dispatch based on type namespace
    if rdf_type.starts_with(SP_NS) {
        handle_spin(line, &rdf_type, store, out);
    } else if rdf_type.starts_with(TOX_NS) {
        match tox {
            Some(t) => handle_tox(line, &rdf_type, t, out),
            None => tracing::warn!(target: "DISPATCH", %rdf_type, "carrier: dispatch skipped (no tox handle)"),
        }
    } else {
        // Unknown type — insert as raw RDF through the DAG
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
                    // Serialize results as Turtle on OUT
                    // For now, emit a simple result indicator
                    use oxigraph::sparql::QueryResults;
                    let mut rows = 0u64;
                    if let QueryResults::Solutions(solutions) = results {
                        for sol in solutions.flatten() {
                            rows += 1;
                            // Emit each binding as a simple triple
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
                            // Also insert constructed triples
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
            // These are SPARQL Update operations
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
// Tox carrier command handling
// ---------------------------------------------------------------------------

fn handle_tox(line: &str, rdf_type: &str, tox: &ToxCarrier, out: &mut dyn AntennaOut) {
    let local = &rdf_type[TOX_NS.len()..];
    match local {
        "GetId" => {
            if let Err(e) = tox.get_id() {
                out.send(&format!(
                    "[] a antenna:Error ; antenna:message \"{}\" .",
                    turtle_escape(&e.to_string())
                ));
            }
        }
        "SetNick" => {
            if let Some(nick) = extract_property(line, "tox:nick")
                .or_else(|| extract_property(line, "carrier:nick"))
            {
                if let Err(e) = tox.set_nick(&nick) {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        "SendMsg" => {
            let friend_id = extract_property(line, "tox:friendId")
                .or_else(|| extract_property(line, "carrier:friendId"))
                .and_then(|s| s.parse::<u32>().ok());
            let text = extract_property(line, "tox:text")
                .or_else(|| extract_property(line, "carrier:text"));

            if let (Some(fid), Some(txt)) = (friend_id, text) {
                if let Err(e) = tox.send_message(fid, &txt) {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        "Save" => {
            if let Err(e) = tox.save() {
                out.send(&format!(
                    "[] a antenna:Error ; antenna:message \"{}\" .",
                    turtle_escape(&e.to_string())
                ));
            }
        }
        "SendFile" => {
            let friend_id = extract_property(line, "tox:friendId")
                .or_else(|| extract_property(line, "carrier:friendId"))
                .and_then(|s| s.parse::<u32>().ok());
            let path = extract_property(line, "tox:path")
                .or_else(|| extract_property(line, "carrier:path"));

            if let (Some(fid), Some(path)) = (friend_id, path) {
                if let Err(e) = tox.send_file(fid, &path) {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            } else {
                out.send(
                    "[] a antenna:Error ; antenna:message \"SendFile missing friendId or path\" .",
                );
            }
        }
        "AcceptFile" => {
            let friend_id = extract_property(line, "carrier:friendId")
                .and_then(|s| s.parse::<u32>().ok());
            let file_id = extract_property(line, "carrier:fileId")
                .and_then(|s| s.parse::<u32>().ok());
            let path = extract_property(line, "carrier:path");

            if let (Some(fid), Some(file_id), Some(path)) = (friend_id, file_id, path) {
                if let Err(e) = tox.accept_file(fid, file_id, &path) {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            } else {
                out.send(
                    "[] a antenna:Error ; antenna:message \"AcceptFile missing friendId, fileId, or path\" .",
                );
            }
        }
        "CancelFile" => {
            let friend_id = extract_property(line, "carrier:friendId")
                .and_then(|s| s.parse::<u32>().ok());
            let file_id = extract_property(line, "carrier:fileId")
                .and_then(|s| s.parse::<u32>().ok());

            if let (Some(fid), Some(file_id)) = (friend_id, file_id) {
                if let Err(e) = tox.cancel_file(fid, file_id) {
                    out.send(&format!(
                        "[] a antenna:Error ; antenna:message \"{}\" .",
                        turtle_escape(&e.to_string())
                    ));
                }
            }
        }
        _ => {
            // Unknown tox command — just insert as data
        }
    }
}

// ---------------------------------------------------------------------------
// Raw RDF insert through the DAG
// ---------------------------------------------------------------------------

fn insert_with_dag(line: &str, store: &RdfStore, dag: &Dag, out: &mut dyn AntennaOut) {
    // Before-insert hooks
    dag.before_insert(line);

    // Insert into store
    if let Err(e) = store.insert_turtle(line) {
        tracing::warn!(target: "SPARQL", %e, "insert error");
        return;
    }

    // After-insert hooks
    dag.after_insert(line);

    // Emit on OUT
    out.send(line);
}

// ---------------------------------------------------------------------------
// Simple Turtle property extraction (lightweight, no full parse)
// ---------------------------------------------------------------------------

pub fn extract_type(line: &str) -> Option<String> {
    // Look for "a <URI>" or "a prefix:local" pattern
    let line = line.trim();

    // Pattern: "[] a <full-uri>" or "[] a prefix:local"
    if let Some(pos) = line.find(" a ") {
        let after = &line[pos + 3..].trim_start();

        // Handle full URIs in angle brackets — find the closing '>'
        if after.starts_with('<') {
            if let Some(end) = after.find('>') {
                return Some(after[1..end].to_string());
            }
            return None;
        }

        // Handle prefixed names — split on delimiters (space, semicolon, dot)
        let type_str = after
            .split([' ', ';', '.'])
            .next()?
            .trim();

        // Resolve common prefixes
        if let Some((prefix, local)) = type_str.split_once(':') {
            let ns = match prefix {
                "sp" => SP_NS,
                "spin" => "http://spinrdf.org/spin#",
                "tox" | "carrier" => TOX_NS,
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
    // Look for 'prop "value"' or 'prop value' patterns
    let search = format!("{} ", prop);
    if let Some(pos) = line.find(&search) {
        let after = &line[pos + search.len()..];
        let after = after.trim();

        if let Some(inner) = after.strip_prefix('"') {
            // Quoted string — find matching close quote (handle escapes)
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
            // Unquoted value (number, boolean, URI)
            let val = after
                .split([' ', ';', '.'])
                .next()?
                .trim();
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

    // -- extract_type --

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
    fn extract_type_prefixed_tox() {
        let line = "[] a tox:SetNick ; tox:nick \"mynode\" .";
        assert_eq!(
            extract_type(line),
            Some("http://resonator.network/v2/carrier#SetNick".to_string())
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

    // -- extract_property --

    #[test]
    fn extract_property_quoted() {
        let line = r#"[] a sp:Select ; sp:text "SELECT ?s WHERE { ?s ?p ?o }" ."#;
        assert_eq!(
            extract_property(line, "sp:text"),
            Some("SELECT ?s WHERE { ?s ?p ?o }".to_string())
        );
    }

    #[test]
    fn extract_property_unquoted_number() {
        let line = "[] a carrier:TextMessage ; carrier:friendId 42 ; carrier:text \"hi\" .";
        assert_eq!(
            extract_property(line, "carrier:friendId"),
            Some("42".to_string())
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

    // -- turtle_escape --

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

    // -- handle_spin tests --

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
        handle_spin(
            line,
            &format!("{}Select", SP_NS),
            &store,
            &mut out,
        );
        let msgs = out.messages();
        assert!(!msgs.is_empty(), "should return at least one result");
        assert!(msgs[0].contains("antenna:Result"), "should be a Result type");
        assert!(msgs[0].contains("hello"), "should contain the value");
    }

    #[test]
    fn handle_spin_select_empty() {
        let store = RdfStore::open(None).unwrap();
        let mut out = TestOut::new();
        let line = r#"[] a sp:Select ; sp:text "SELECT ?s WHERE { ?s a <urn:Nothing> }" ."#;
        handle_spin(
            line,
            &format!("{}Select", SP_NS),
            &store,
            &mut out,
        );
        assert!(out.messages().is_empty(), "no results expected");
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
        // ISSUE-078 regression: a DELETE WHERE + INSERT DATA via sp:Modify
        // must actually mutate the store when it arrives through dispatch
        // (previously the emit path inserted the Modify turtle as data).
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

    // -- insert_with_dag tests --

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
        // Should not crash; error is logged via eprintln
        assert!(out.messages().is_empty());
    }
}
