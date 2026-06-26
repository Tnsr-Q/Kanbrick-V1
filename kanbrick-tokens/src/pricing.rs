//! Per-bucket token pricing (P9.5, #105).
//!
//! A [`ModelPrice`] carries a **nano-USD-per-token** rate for each of the five
//! disjoint [`Usage`] buckets, so pricing a call is a per-bucket sum — the
//! disjointness from P9.1 is exactly what lets cache reads bill cheaper than fresh
//! input without any double-count. A [`PriceTable`] resolves `(provider, model)`
//! to a price, falling back to a per-provider default.

use std::collections::HashMap;

use kanbrick_providers::{ProviderKind, Usage};
use serde::{Deserialize, Serialize};

/// Per-token price, in **nano-USD** (`1e-9` USD), for each disjoint [`Usage`]
/// bucket. A vendor's "USD per 1M tokens" rate converts by `× 1000`
/// (`$3.00 / 1M` → `3000`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelPrice {
    /// nano-USD per uncached input token.
    pub input: u64,
    /// nano-USD per visible output token.
    pub output: u64,
    /// nano-USD per cache-read (cache-hit) input token — usually a fraction of `input`.
    pub cache_read: u64,
    /// nano-USD per cache-write token — usually a premium over `input`.
    pub cache_write: u64,
    /// nano-USD per hidden reasoning token — usually billed at the `output` rate.
    pub reasoning: u64,
}

impl ModelPrice {
    /// The cost, in nano-USD, of `usage` under this price.
    ///
    /// Each disjoint bucket is multiplied by its own rate and summed; everything
    /// saturates, so an adversarial token count can never wrap a total.
    pub fn cost_nano(&self, usage: &Usage) -> u64 {
        let mul = |tokens: u64, rate: u64| tokens.saturating_mul(rate);
        mul(usage.input, self.input)
            .saturating_add(mul(usage.output, self.output))
            .saturating_add(mul(usage.cache_read, self.cache_read))
            .saturating_add(mul(usage.cache_write, self.cache_write))
            .saturating_add(mul(usage.reasoning, self.reasoning))
    }
}

/// Resolves `(provider, model)` to a [`ModelPrice`].
///
/// Lookup order: an exact `(provider, model)` entry, then the provider's default,
/// then `None` (an unknown model with no provider default is a pricing miss the
/// caller must handle — the ledger surfaces it as `LedgerError::NoPrice`).
#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    by_model: HashMap<(ProviderKind, String), ModelPrice>,
    default_by_provider: HashMap<ProviderKind, ModelPrice>,
}

impl PriceTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the price for an exact `(provider, model)` (builder).
    pub fn with_model(
        mut self,
        provider: ProviderKind,
        model: impl Into<String>,
        price: ModelPrice,
    ) -> Self {
        self.by_model.insert((provider, model.into()), price);
        self
    }

    /// Set the fallback price for any of `provider`'s models without an exact entry
    /// (builder).
    pub fn with_default(mut self, provider: ProviderKind, price: ModelPrice) -> Self {
        self.default_by_provider.insert(provider, price);
        self
    }

    /// Resolve a price, or `None` on a miss.
    pub fn price(&self, provider: ProviderKind, model: &str) -> Option<ModelPrice> {
        self.by_model
            .get(&(provider, model.to_string()))
            .copied()
            .or_else(|| self.default_by_provider.get(&provider).copied())
    }

    /// The cost, in nano-USD, of `usage` for `(provider, model)`, or `None` on a
    /// pricing miss.
    pub fn cost_nano(&self, provider: ProviderKind, model: &str, usage: &Usage) -> Option<u64> {
        self.price(provider, model).map(|p| p.cost_nano(usage))
    }

    /// An **illustrative** default table with rough public list prices, expressed
    /// as nano-USD/token.
    ///
    /// These are convenience defaults for development, **not** authoritative
    /// billing rates — vendor prices change and vary by tier/region. Override with
    /// [`with_model`](Self::with_model) before billing anyone. Each provider also
    /// gets a default so an unrecognized model still prices rather than erroring.
    pub fn illustrative() -> Self {
        // nano-USD/token == (USD per 1M tokens) × 1000.
        // Anthropic claude-opus: in $15 / out $75 / cache-read $1.50 / cache-write $18.75.
        let opus = ModelPrice {
            input: 15_000,
            output: 75_000,
            cache_read: 1_500,
            cache_write: 18_750,
            reasoning: 75_000,
        };
        // OpenAI gpt-4o: in $2.50 / out $10 / cache-read $1.25 / no cache-write.
        let gpt4o = ModelPrice {
            input: 2_500,
            output: 10_000,
            cache_read: 1_250,
            cache_write: 0,
            reasoning: 10_000,
        };
        // Cerebras llama: flat ~$0.60 in/out, no prompt cache billing.
        let cerebras = ModelPrice {
            input: 600,
            output: 600,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        PriceTable::new()
            .with_model(ProviderKind::Anthropic, "claude-opus-4-8", opus)
            .with_default(ProviderKind::Anthropic, opus)
            .with_model(ProviderKind::OpenAI, "gpt-4o", gpt4o)
            .with_default(ProviderKind::OpenAI, gpt4o)
            .with_default(ProviderKind::Cerebras, cerebras)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> Usage {
        Usage {
            input: 1_000,
            output: 500,
            cache_read: 2_000,
            cache_write: 100,
            reasoning: 50,
        }
    }

    #[test]
    fn cost_prices_each_disjoint_bucket_independently() {
        let price = ModelPrice {
            input: 10,
            output: 30,
            cache_read: 1,
            cache_write: 12,
            reasoning: 30,
        };
        // 1000*10 + 500*30 + 2000*1 + 100*12 + 50*30 = 10000+15000+2000+1200+1500.
        assert_eq!(price.cost_nano(&usage()), 29_700);
    }

    #[test]
    fn cost_saturates_rather_than_wrapping() {
        let price = ModelPrice {
            input: u64::MAX,
            ..ModelPrice::default()
        };
        let u = Usage {
            input: 2,
            ..Usage::default()
        };
        assert_eq!(price.cost_nano(&u), u64::MAX);
    }

    #[test]
    fn table_prefers_exact_model_then_provider_default() {
        let exact = ModelPrice {
            input: 5,
            ..ModelPrice::default()
        };
        let fallback = ModelPrice {
            input: 1,
            ..ModelPrice::default()
        };
        let table = PriceTable::new()
            .with_model(ProviderKind::OpenAI, "gpt-4o", exact)
            .with_default(ProviderKind::OpenAI, fallback);
        assert_eq!(table.price(ProviderKind::OpenAI, "gpt-4o"), Some(exact));
        // An unknown OpenAI model falls back to the provider default.
        assert_eq!(
            table.price(ProviderKind::OpenAI, "o9-future"),
            Some(fallback)
        );
        // A provider with no entry at all is a miss.
        assert_eq!(table.price(ProviderKind::Cerebras, "llama"), None);
    }

    #[test]
    fn illustrative_table_prices_known_and_unknown_models() {
        let table = PriceTable::illustrative();
        let u = Usage {
            input: 1_000_000,
            ..Usage::default()
        };
        // 1M input tokens of gpt-4o at $2.50/1M == $2.50 == 2.5e9 nano-USD.
        assert_eq!(
            table.cost_nano(ProviderKind::OpenAI, "gpt-4o", &u),
            Some(2_500_000_000)
        );
        // Unknown Anthropic model still prices via the provider default.
        assert!(table
            .cost_nano(ProviderKind::Anthropic, "claude-future", &u)
            .is_some());
    }
}
