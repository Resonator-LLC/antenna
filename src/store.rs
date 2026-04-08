// Copyright (c) 2025-2026 Resonator LLC. Licensed under MIT.

//! Thin wrapper around embedded Oxigraph store.
use anyhow::Result;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::NamedNodeRef;
use oxigraph::sparql::QueryResults;
use oxigraph::store::Store;

use crate::carrier_tox::TURTLE_PREFIXES;

pub struct RdfStore {
    store: Store,
}

impl RdfStore {
    pub fn open(path: Option<&str>) -> Result<Self> {
        let store = match path {
            Some(p) => Store::open(p)?,
            None => Store::new()?,
        };
        Ok(Self { store })
    }

    /// Parse a Turtle string and insert its triples into the default graph.
    /// Carrier/antenna prefixes are automatically prepended.
    pub fn insert_turtle(&self, turtle: &str) -> Result<()> {
        let doc = format!("{}{}\n", TURTLE_PREFIXES, turtle);
        let parser = RdfParser::from_format(RdfFormat::Turtle);
        self.store.load_from_reader(parser, doc.as_bytes())?;
        Ok(())
    }

    /// Insert Turtle triples into a specific named graph.
    pub fn insert_turtle_to_graph(&self, turtle: &str, graph: &str) -> Result<()> {
        let doc = format!("{}{}\n", TURTLE_PREFIXES, turtle);
        let parser = RdfParser::from_format(RdfFormat::Turtle);
        let graph_name = NamedNodeRef::new(graph)?;
        self.store
            .load_from_reader(parser.with_default_graph(graph_name), doc.as_bytes())?;
        Ok(())
    }

    /// Remove all triples in a named graph.
    pub fn clear_graph(&self, graph: &str) -> Result<()> {
        let graph_name = NamedNodeRef::new(graph)?;
        self.store.clear_graph(graph_name)?;
        Ok(())
    }

    /// Run a SPARQL query. Returns the raw QueryResults for the caller to iterate.
    pub fn query(&self, sparql: &str) -> Result<QueryResults> {
        Ok(self.store.query(sparql)?)
    }

    /// Run a SPARQL ASK query. Returns true/false.
    pub fn ask(&self, sparql: &str) -> Result<bool> {
        match self.store.query(sparql)? {
            QueryResults::Boolean(b) => Ok(b),
            _ => Ok(false),
        }
    }

    /// Run a SPARQL UPDATE (INSERT DATA, DELETE DATA, DELETE/INSERT WHERE).
    pub fn update(&self, sparql: &str) -> Result<()> {
        self.store.update(sparql)?;
        Ok(())
    }

    /// Access the underlying store (for advanced operations).
    pub fn inner(&self) -> &Store {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_store() -> RdfStore {
        RdfStore::open(None).unwrap()
    }

    #[test]
    fn insert_and_ask() {
        let store = mem_store();
        store.insert_turtle("<urn:s> a <urn:Type> .").unwrap();
        assert!(store.ask("ASK { <urn:s> a <urn:Type> }").unwrap());
    }

    #[test]
    fn insert_and_select() {
        let store = mem_store();
        store
            .insert_turtle("<urn:a> <urn:name> \"Alice\" .")
            .unwrap();
        let results = store
            .query("SELECT ?name WHERE { <urn:a> <urn:name> ?name }")
            .unwrap();
        if let QueryResults::Solutions(solutions) = results {
            let rows: Vec<_> = solutions.filter_map(|s| s.ok()).collect();
            assert_eq!(rows.len(), 1);
        } else {
            panic!("expected Solutions");
        }
    }

    #[test]
    fn named_graph_isolation() {
        let store = mem_store();
        store
            .insert_turtle_to_graph("<urn:x> a <urn:Foo> .", "urn:graph:test")
            .unwrap();

        // Not in default graph
        assert!(!store.ask("ASK { <urn:x> a <urn:Foo> }").unwrap());

        // In named graph
        assert!(store
            .ask("ASK { GRAPH <urn:graph:test> { <urn:x> a <urn:Foo> } }")
            .unwrap());
    }

    #[test]
    fn clear_graph() {
        let store = mem_store();
        store
            .insert_turtle_to_graph("<urn:y> a <urn:Bar> .", "urn:graph:tmp")
            .unwrap();
        assert!(store
            .ask("ASK { GRAPH <urn:graph:tmp> { <urn:y> a <urn:Bar> } }")
            .unwrap());

        store.clear_graph("urn:graph:tmp").unwrap();
        assert!(!store
            .ask("ASK { GRAPH <urn:graph:tmp> { <urn:y> a <urn:Bar> } }")
            .unwrap());
    }

    #[test]
    fn update_insert_data() {
        let store = mem_store();
        store.update("INSERT DATA { <urn:z> a <urn:Baz> }").unwrap();
        assert!(store.ask("ASK { <urn:z> a <urn:Baz> }").unwrap());
    }

    #[test]
    fn update_delete_data() {
        let store = mem_store();
        store.insert_turtle("<urn:d> a <urn:Del> .").unwrap();
        store.update("DELETE DATA { <urn:d> a <urn:Del> }").unwrap();
        assert!(!store.ask("ASK { <urn:d> a <urn:Del> }").unwrap());
    }

    #[test]
    fn ask_returns_false_for_empty() {
        let store = mem_store();
        assert!(!store.ask("ASK { <urn:nothing> a <urn:Nothing> }").unwrap());
    }

    #[test]
    fn insert_turtle_with_prefixes() {
        let store = mem_store();
        // carrier: prefix is auto-prepended
        store
            .insert_turtle("[] a carrier:Connected ; carrier:transport \"UDP\" .")
            .unwrap();
        assert!(store
            .ask("PREFIX carrier: <http://resonator.network/v2/carrier#> ASK { ?s a carrier:Connected }")
            .unwrap());
    }
}
