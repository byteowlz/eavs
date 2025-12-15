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
mod word_lists;

// Public API
pub use cost::CostCalculator;
pub use generation::is_virtual_key;
pub use pricing::SharedPricingTable;
pub use rate_limit::RateLimiter;
pub use store::{KeyStore, UsageRecord};
pub use types::*;
pub use validation::{KeyValidator, ValidatedKey};
