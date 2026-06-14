//! Bridging between SparrowDB's [`Value`] and serde.
//!
//! The store layer accepts query parameters as a [`Params`] map and deserializes
//! result rows into typed Rust structs. Both directions go through
//! `serde_json::Value` as a neutral intermediate: parameters are JSON values
//! lowered into SparrowDB [`Value`]s, and result cells are lifted from
//! SparrowDB [`Value`]s back into JSON before `serde` deserialization.

use std::collections::HashMap;

use sparrowdb_execution::types::Value;

/// A set of named query parameters bound into a Cypher statement.
///
/// Parameters are *bound*, never interpolated into the query text, so a value
/// like `"x' OR '1'='1"` is treated as an opaque string and cannot alter the
/// parsed structure of the query (injection prevention, issue #9).
#[derive(Debug, Default, Clone)]
pub struct Params {
    inner: HashMap<String, Value>,
}

impl Params {
    /// An empty parameter set.
    pub fn new() -> Self {
        Params::default()
    }

    /// Bind `name` to `value`, returning `self` for chaining.
    pub fn with(mut self, name: impl Into<String>, value: impl Into<ParamValue>) -> Self {
        self.inner.insert(name.into(), value.into().0);
        self
    }

    /// Bind `name` to `value` in place.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<ParamValue>) {
        self.inner.insert(name.into(), value.into().0);
    }

    /// Whether any parameters are bound.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Consume into the `HashMap` SparrowDB's `execute_with_params` expects.
    pub(crate) fn into_map(self) -> HashMap<String, Value> {
        self.inner
    }
}

/// A single bound parameter value. Constructed via the [`From`] impls below so
/// callers can write `params.with("email", person.email)` for common types.
pub struct ParamValue(Value);

impl From<Value> for ParamValue {
    fn from(v: Value) -> Self {
        ParamValue(v)
    }
}

impl From<&str> for ParamValue {
    fn from(v: &str) -> Self {
        ParamValue(Value::String(v.to_string()))
    }
}

impl From<String> for ParamValue {
    fn from(v: String) -> Self {
        ParamValue(Value::String(v))
    }
}

impl From<&String> for ParamValue {
    fn from(v: &String) -> Self {
        ParamValue(Value::String(v.clone()))
    }
}

impl From<i64> for ParamValue {
    fn from(v: i64) -> Self {
        ParamValue(Value::Int64(v))
    }
}

impl From<i32> for ParamValue {
    fn from(v: i32) -> Self {
        ParamValue(Value::Int64(v as i64))
    }
}

impl From<f64> for ParamValue {
    fn from(v: f64) -> Self {
        ParamValue(Value::Float64(v))
    }
}

impl From<bool> for ParamValue {
    fn from(v: bool) -> Self {
        ParamValue(Value::Bool(v))
    }
}

/// Lift a SparrowDB [`Value`] into a `serde_json::Value`.
///
/// Strings, numbers, booleans, and null map directly. Node/edge references
/// surface as their numeric id. Lists and maps recurse; vectors become arrays
/// of numbers.
pub(crate) fn value_to_json(value: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        Value::Null => J::Null,
        Value::Int64(i) => J::from(*i),
        Value::Float64(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
        Value::Bool(b) => J::Bool(*b),
        Value::String(s) => J::String(s.clone()),
        Value::NodeRef(id) => J::from(id.0),
        Value::EdgeRef(id) => J::from(id.0),
        Value::List(items) => J::Array(items.iter().map(value_to_json).collect()),
        Value::Map(entries) => J::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Vector(xs) => J::Array(
            xs.iter()
                .map(|x| serde_json::Number::from_f64(*x as f64).map_or(J::Null, J::Number))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_lift_to_json() {
        assert_eq!(value_to_json(&Value::Int64(42)), serde_json::json!(42));
        assert_eq!(
            value_to_json(&Value::String("hi".into())),
            serde_json::json!("hi")
        );
        assert_eq!(value_to_json(&Value::Bool(true)), serde_json::json!(true));
        assert_eq!(value_to_json(&Value::Null), serde_json::Value::Null);
    }

    #[test]
    fn params_lower_common_types() {
        let p = Params::new()
            .with("email", "a@b.com")
            .with("age", 30i64)
            .with("active", true);
        let map = p.into_map();
        assert_eq!(map.get("email"), Some(&Value::String("a@b.com".into())));
        assert_eq!(map.get("age"), Some(&Value::Int64(30)));
        assert_eq!(map.get("active"), Some(&Value::Bool(true)));
    }
}
