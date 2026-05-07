//! Proxy transport for routing LLM calls through a server that holds provider
//! credentials.
//!
//! The proxy forwards model requests to an HTTP endpoint that performs the
//! actual provider authentication, and streams events back to the client. As
//! part of that streaming, it strips the `partial` field from delta events so
//! that downstream consumers see a normalized event shape.
//!
//! Mirrors the TypeScript implementation at
//! `pi-mono/packages/agent/src/proxy.ts`.
//!
//! Scaffold-only: types and functions land in subsequent tasks of the
//! agent-proxy port (see `docs/exec-plans/agent-proxy-port.md`).
