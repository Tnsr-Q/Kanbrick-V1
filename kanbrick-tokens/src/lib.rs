//! Priced token ledger + budget primitives for BYO-AI usage (L5 Cockpit, P9.5 — #105).
//!
//! Every BYO-AI call returns a normalized, **disjoint** [`Usage`] (P9.1): `input`,
//! `output`, `cache_read`, `cache_write`, and `reasoning` are mutually exclusive
//! token buckets. This crate is where that disjointness pays off — each bucket is
//! **priced independently** (a cache-read token can cost a tenth of an input token;
//! a cache-write token more), so a cost is an honest per-bucket sum with no
//! double-counting. P9.5 *captures*: it prices a `Usage` and records it to a
//! per-employee ledger, and it defines the [`Budget`] value type. **Enforcement**
//! — the central approval queue and the sweep that rejects over-budget spend —
//! is P12.3 (ADR-0015), built on these primitives; this crate does not gate calls.
//!
//! ## Units
//!
//! Costs are integer **nano-USD** (`1e-9` USD) end-to-end — no floats in the hot
//! path, so totals are exact and cannot drift. Vendors quote "USD per 1M tokens";
//! that converts to nano-USD *per token* by `× 1000` (e.g. `$3.00 / 1M` = `3000`
//! nano-USD/token). See [`ModelPrice`] and [`nanos_to_usd_string`].
//!
//! ## Modules
//!
//! - [`pricing`] — [`ModelPrice`] (per-bucket nano-USD rates) + [`PriceTable`]
//!   (per-`(provider, model)` lookup with a per-provider fallback).
//! - [`ledger`] — [`LedgerEntry`], the [`TokenLedger`] trait, and an in-memory
//!   backend, namespaced by `user_id` like provider-key custody.
//! - [`budget`] — the [`Budget`] value type + [`BudgetStatus`] (the foundation
//!   P12.3 enforces against).
//!
//! [`Usage`]: kanbrick_providers::Usage

pub mod budget;
pub mod ledger;
pub mod pricing;

pub use budget::{Budget, BudgetStatus};
pub use ledger::{InMemoryLedger, LedgerEntry, LedgerError, TokenLedger};
pub use pricing::{ModelPrice, PriceTable};

/// Render a nano-USD amount as a `$`-prefixed decimal string with full nano
/// precision (`3_000_000_000` → `"$3.000000000"`). Display only — never used for
/// arithmetic, which stays in integer nano-USD.
pub fn nanos_to_usd_string(nano_usd: u64) -> String {
    let whole = nano_usd / 1_000_000_000;
    let frac = nano_usd % 1_000_000_000;
    format!("${whole}.{frac:09}")
}

/// Current time in Unix seconds, saturating rather than panicking on a clock
/// before the epoch or far in the future. Used to stamp ledger entries.
pub(crate) fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_to_usd_string_formats_with_nano_precision() {
        assert_eq!(nanos_to_usd_string(3_000_000_000), "$3.000000000");
        assert_eq!(nanos_to_usd_string(2_500), "$0.000002500");
        assert_eq!(nanos_to_usd_string(0), "$0.000000000");
    }
}
