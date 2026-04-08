# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-04-05

### Added
- LLM backend integration with SemanticRouter DAG nodes
- Ollama, OpenAI-compatible HTTP, and Platform (on-device) backends
- Multi-client WebSocket server with greeting per client
- Unit tests for dispatch, LLM helpers, and store operations
- Example pipeline definitions (echo, filter, enrich, semantic router)
- Builder pattern for AntennaContext
- ARCHITECTURE.md, CONTRIBUTING.md, LICENSE files
- SAFETY comments on all unsafe blocks
- Environment variable overrides for build.rs dependency paths

### Changed
- README overhauled with architecture diagram, protocol examples, and DAG docs

## [0.1.0] - 2026-03-25

### Added
- Initial prototype: Tox P2P carrier, Oxigraph store, QuickJS scripting DAG
- SPIN/SPARQL query dispatch
- Pipe-based stdin/stdout transport
- Channel-based DAG with thread-per-node execution
- Clock signaling (eventfd on Linux, self-pipe on macOS)
