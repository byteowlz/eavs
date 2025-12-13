//! Virtual API Keys system.
//!
//! Provides secure virtual API keys that can be issued to applications,
//! with scoping (models, providers, rate limits, budgets) and usage tracking.

mod cost;
mod generation;
mod pricing;
mod rate_limit;
mod store;
mod types;
mod validation;

// Public API - some items may not be used internally but are exposed for external use
pub use cost::CostCalculator;
pub use generation::is_virtual_key;
pub use pricing::SharedPricingTable;
pub use rate_limit::RateLimiter;
pub use store::{KeyStore, UsageRecord};
pub use types::*;
pub use validation::{KeyValidator, ValidatedKey};
