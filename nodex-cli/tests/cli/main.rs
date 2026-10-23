//! CLI contract tests: each test runs the `nodex` binary against a
//! temporary project and asserts what the command promises — the JSON
//! envelope, the exit status, the error classification and the files it
//! writes.
//!
//! A test lives in the module of the command whose behaviour it asserts
//! (`rename`, `scaffold`, `query`, …). A behaviour that holds across
//! commands has a module of its own (`envelope`, `baseline`, `locks`,
//! `status_flow`, `write_gate`, `scope`, …), and a test asserting it lives
//! there whichever command it drives. A helper used by one module lives in
//! it; one used by more lives in `support`. Each test is self-contained:
//! no shared mutable state, no ordering.

mod agent_scenarios;
mod baseline;
mod build;
mod check;
mod check_content;
mod checkout;
mod config;
mod containment;
mod contract_gate;
mod diff;
mod docs;
mod envelope;
mod export;
mod init;
mod lifecycle;
mod locks;
mod migrate;
mod paths;
mod query;
mod rename;
mod repoint;
mod report;
mod retarget;
mod scaffold;
mod scope;
mod snapshot;
mod status_flow;
mod support;
mod write_gate;
