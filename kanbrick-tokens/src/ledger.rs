//! The per-employee token ledger (P9.5, #105).
//!
//! Records one priced [`LedgerEntry`] per BYO-AI call, namespaced by `user_id`
//! (like provider-key custody, ADR-0009) so one employee's spend is never mixed
//! with another's. Aggregation folds the disjoint [`Usage`] with the very
//! [`Usage::accumulate`] P9.1 defined for streaming, so per-user totals stay exact
//! and disjoint. Enforcement lives in P12.3 — this is capture and query only.

use std::collections::HashMap;
use std::sync::Mutex;

use kanbrick_providers::{ProviderKind, Usage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pricing::PriceTable;

/// One recorded, priced BYO-AI call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Entry identifier.
    pub id: Uuid,
    /// The employee who made the call (the namespacing key).
    pub user_id: Uuid,
    /// Which provider was billed.
    pub provider: ProviderKind,
    /// The model id used.
    pub model: String,
    /// The normalized, disjoint token usage.
    pub usage: Usage,
    /// The computed cost in nano-USD (`1e-9` USD).
    pub cost_nano_usd: u64,
    /// When the call was recorded, Unix seconds.
    pub at: i64,
}

/// A ledger failure.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The backend (lock, store) failed.
    #[error("ledger backend error: {0}")]
    Backend(String),
    /// No price could be resolved for the call, so it cannot be recorded priced.
    #[error("no price for provider {provider} model {model}")]
    NoPrice {
        /// The provider with no matching price.
        provider: ProviderKind,
        /// The model with no matching price.
        model: String,
    },
}

/// Captures and queries per-employee BYO-AI spend.
///
/// Object-safe and `Send + Sync` so the app can hold `Arc<dyn TokenLedger>` and
/// swap a durable (SparrowDB-backed) ledger in later. Every method is scoped to a
/// `user_id`.
pub trait TokenLedger: Send + Sync {
    /// Append a fully-formed entry.
    fn record(&self, entry: LedgerEntry) -> Result<(), LedgerError>;

    /// Every entry for `user_id`, in record order.
    fn entries(&self, user_id: Uuid) -> Result<Vec<LedgerEntry>, LedgerError>;

    /// The disjoint sum of every recorded [`Usage`] for `user_id`.
    fn total_usage(&self, user_id: Uuid) -> Result<Usage, LedgerError>;

    /// The total nano-USD spent by `user_id` (saturating).
    fn total_cost_nano(&self, user_id: Uuid) -> Result<u64, LedgerError>;

    /// Price `usage` via `pricing` and record it, returning the stored entry.
    ///
    /// A convenience over [`record`](Self::record): the call site hands over the
    /// `Usage` from a completion/stream outcome and the table, and the ledger
    /// stamps the id/time and computes the cost. A pricing miss is
    /// [`LedgerError::NoPrice`] (nothing is recorded).
    fn record_usage(
        &self,
        pricing: &PriceTable,
        user_id: Uuid,
        provider: ProviderKind,
        model: &str,
        usage: Usage,
    ) -> Result<LedgerEntry, LedgerError> {
        let cost_nano_usd =
            pricing
                .cost_nano(provider, model, &usage)
                .ok_or_else(|| LedgerError::NoPrice {
                    provider,
                    model: model.to_string(),
                })?;
        let entry = LedgerEntry {
            id: Uuid::new_v4(),
            user_id,
            provider,
            model: model.to_string(),
            usage,
            cost_nano_usd,
            at: crate::now_unix(),
        };
        self.record(entry.clone())?;
        Ok(entry)
    }
}

/// Per-user ledger entries (`user_id -> entries in record order`). Aliased so the
/// map type stays under clippy's `type_complexity` bar where it appears in the
/// `Mutex` field and the `lock()` guard return.
type LedgerStore = HashMap<Uuid, Vec<LedgerEntry>>;

/// A process-memory [`TokenLedger`]: per-user entry lists behind a `Mutex`. The
/// CI-testable backend and the reference for the namespacing invariant; a durable
/// backend implements the same trait later.
#[derive(Default)]
pub struct InMemoryLedger {
    inner: Mutex<LedgerStore>,
}

impl InMemoryLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, LedgerStore>, LedgerError> {
        self.inner
            .lock()
            .map_err(|_| LedgerError::Backend("in-memory ledger lock poisoned".to_string()))
    }
}

impl TokenLedger for InMemoryLedger {
    fn record(&self, entry: LedgerEntry) -> Result<(), LedgerError> {
        self.lock()?.entry(entry.user_id).or_default().push(entry);
        Ok(())
    }

    fn entries(&self, user_id: Uuid) -> Result<Vec<LedgerEntry>, LedgerError> {
        Ok(self.lock()?.get(&user_id).cloned().unwrap_or_default())
    }

    fn total_usage(&self, user_id: Uuid) -> Result<Usage, LedgerError> {
        let guard = self.lock()?;
        let mut total = Usage::default();
        if let Some(entries) = guard.get(&user_id) {
            for entry in entries {
                total.accumulate(&entry.usage);
            }
        }
        Ok(total)
    }

    fn total_cost_nano(&self, user_id: Uuid) -> Result<u64, LedgerError> {
        let guard = self.lock()?;
        Ok(guard
            .get(&user_id)
            .map(|entries| {
                entries
                    .iter()
                    .fold(0u64, |acc, e| acc.saturating_add(e.cost_nano_usd))
            })
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            ..Usage::default()
        }
    }

    #[test]
    fn record_usage_prices_and_appends() {
        let ledger = InMemoryLedger::new();
        let table = PriceTable::new().with_default(
            ProviderKind::OpenAI,
            crate::ModelPrice {
                input: 10,
                output: 20,
                ..crate::ModelPrice::default()
            },
        );
        let user = Uuid::new_v4();
        let entry = ledger
            .record_usage(&table, user, ProviderKind::OpenAI, "gpt-4o", usage(100, 50))
            .unwrap();
        // 100*10 + 50*20 = 2000.
        assert_eq!(entry.cost_nano_usd, 2_000);
        assert_eq!(entry.provider, ProviderKind::OpenAI);
        assert_eq!(ledger.entries(user).unwrap().len(), 1);
        assert_eq!(ledger.total_cost_nano(user).unwrap(), 2_000);
    }

    #[test]
    fn record_usage_without_a_price_errors_and_records_nothing() {
        let ledger = InMemoryLedger::new();
        let user = Uuid::new_v4();
        let err = ledger
            .record_usage(
                &PriceTable::new(),
                user,
                ProviderKind::Cerebras,
                "llama",
                usage(1, 1),
            )
            .unwrap_err();
        assert!(matches!(err, LedgerError::NoPrice { .. }));
        assert!(ledger.entries(user).unwrap().is_empty());
    }

    #[test]
    fn totals_aggregate_disjoint_usage_across_calls() {
        let ledger = InMemoryLedger::new();
        let table = PriceTable::new().with_default(
            ProviderKind::Anthropic,
            crate::ModelPrice {
                input: 1,
                output: 1,
                ..crate::ModelPrice::default()
            },
        );
        let user = Uuid::new_v4();
        ledger
            .record_usage(
                &table,
                user,
                ProviderKind::Anthropic,
                "claude",
                usage(100, 10),
            )
            .unwrap();
        ledger
            .record_usage(
                &table,
                user,
                ProviderKind::Anthropic,
                "claude",
                usage(50, 90),
            )
            .unwrap();
        let total = ledger.total_usage(user).unwrap();
        assert_eq!(total.input, 150);
        assert_eq!(total.output, 100);
        assert_eq!(total.total(), 250);
        assert_eq!(ledger.total_cost_nano(user).unwrap(), 250); // (110 + 140) * 1
    }

    #[test]
    fn spend_is_namespaced_per_user() {
        let ledger: Arc<dyn TokenLedger> = Arc::new(InMemoryLedger::new());
        let table = PriceTable::new().with_default(
            ProviderKind::OpenAI,
            crate::ModelPrice {
                input: 1,
                ..crate::ModelPrice::default()
            },
        );
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        ledger
            .record_usage(
                &table,
                alice,
                ProviderKind::OpenAI,
                "gpt-4o",
                usage(1_000, 0),
            )
            .unwrap();
        // Bob's view is empty; Alice's spend is hers alone.
        assert!(ledger.entries(bob).unwrap().is_empty());
        assert_eq!(ledger.total_cost_nano(bob).unwrap(), 0);
        assert_eq!(ledger.total_cost_nano(alice).unwrap(), 1_000);
    }
}
