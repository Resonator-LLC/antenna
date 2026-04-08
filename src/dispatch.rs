///! Reactive router: parse incoming Turtle, dispatch by rdf:type.

use anyhow::Result;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{NamedNodeRef, Term};
use oxigraph::store::Store;

use crate::carrier_tox::ToxCarrier;
use crate::channel::AntennaOut;
use crate::dag::Dag;
use crate::store::RdfStore;

const SP_NS: &str = "http://spinrdf.org/sp#";
const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const TOX_NS: &str = "http://resonator.network/v2/carrier#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Dispatch a single Turtle line based on its rdf:type.
pub fn dispatch(
    line: &str,
    store: &RdfStore,
    dag: &Dag,
    tox: &ToxCarrier,
    out: &mut dyn AntennaOut,
) {
    if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
        return;
    }

    // Try to extract the rdf:type from the Turtle statement
    let rdf_type = match extract_type(line) {
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
        handle_tox(line, &rdf_type, tox, out);
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
        "Select" => match store.query(&sparql) {
            Ok(results) => {
                // Serialize results as Turtle on OUT
                // For now, emit a simple result indicator
                use oxigraph::sparql::QueryResults;
                if let QueryResults::Solutions(solutions) = results {
                    for solution in solutions {
                        if let Ok(sol) = solution {
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
                }
            }
            Err(e) => {
                out.send(&format!(
                    "[] a antenna:Error ; antenna:message \"{}\" .",
                    turtle_escape(&e.to_string())
                ));
            }
        },
        "Ask" => match store.ask(&sparql) {
            Ok(b) => {
                out.send(&format!("[] a sp:AskResult ; sp:boolean {} .", b));
            }
            Err(e) => {
                out.send(&format!(
                    "[] a antenna:Error ; antenna:message \"{}\" .",
                    turtle_escape(&e.to_string())
                ));
            }
        },
        "Construct" => match store.query(&sparql) {
            Ok(results) => {
                use oxigraph::sparql::QueryResults;
                if let QueryResults::Graph(triples) = results {
                    for triple in triples {
                        if let Ok(t) = triple {
                            let turtle = format!("{} {} {} .", t.subject, t.predicate, t.object);
                            out.send(&turtle);
                            // Also insert constructed triples
                            let _ = store.insert_turtle(&turtle);
                        }
                    }
                }
            }
            Err(e) => {
                out.send(&format!(
                    "[] a antenna:Error ; antenna:message \"{}\" .",
                    turtle_escape(&e.to_string())
                ));
            }
        },
        "InsertData" | "DeleteData" | "Modify" => {
            // These are SPARQL Update operations
            match store.update(&sparql) {
                Ok(()) => {}
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
        eprintln!("antenna: insert error: {}", e);
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

fn extract_type(line: &str) -> Option<String> {
    // Look for "a <URI>" or "a prefix:local" pattern
    let line = line.trim();

    // Pattern: "[] a <full-uri>" or "[] a prefix:local"
    if let Some(pos) = line.find(" a ") {
        let after = &line[pos + 3..];
        let type_str = after
            .split(|c: char| c == ' ' || c == ';' || c == '.')
            .next()?
            .trim();

        if type_str.starts_with('<') && type_str.ends_with('>') {
            return Some(type_str[1..type_str.len() - 1].to_string());
        }

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

fn extract_property(line: &str, prop: &str) -> Option<String> {
    // Look for 'prop "value"' or 'prop value' patterns
    let search = format!("{} ", prop);
    if let Some(pos) = line.find(&search) {
        let after = &line[pos + search.len()..];
        let after = after.trim();

        if after.starts_with('"') {
            // Quoted string — find matching close quote (handle escapes)
            let inner = &after[1..];
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
                .split(|c: char| c == ' ' || c == ';' || c == '.')
                .next()?
                .trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }

    None
}

fn turtle_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
