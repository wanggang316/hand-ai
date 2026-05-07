# Architecture Decision Records

This directory contains the architectural decision records (ADRs) for `hand-coding-agent`. Each ADR captures one decision, the alternatives considered, and the rationale, in enough detail that a future engineer (or fresh agent) can reconstruct the reasoning without spelunking through chat logs.

Format: lightweight Markdown, one file per decision, numbered sequentially.

## Index

- [ADR-001: Extensions Runtime Architecture](./0001-extensions-runtime.md) — Accepted, 2026-05-07. Hybrid two-tier model: Tier 1 compiled-in Rust trait extensions for the deep-integration case (~85% of pi-mono use cases), Tier 2 subprocess JSON-RPC extensions for polyglot and process-isolated cases (~15%). WASM and framebuffer-RPC tiers deferred to Phase 6+.
