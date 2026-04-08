///! Thin wrapper around embedded Oxigraph store.

use anyhow::Result;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphNameRef, NamedNodeRef};
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
        self.store
            .load_from_reader(parser, doc.as_bytes())?;
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
