//! Headless core of the eavs console (ADR-0022). No gpui: the instance
//! registry, the operator API client, provider presets and presentation rules
//! live here so every front end (this app, tests, a future CLI) shares them.

pub mod client;
pub mod instances;
pub mod presets;
pub mod view;

pub use client::{
    AccountUsage, ApiError, Availability, Client, Meter, NewProvider, ProbeResult, ProviderSource,
};
pub use instances::{DeclaredInstance, Instance, Origin, SecretRef};
pub use presets::{Auth, PRESETS, Preset};
