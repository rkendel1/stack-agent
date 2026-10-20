//! A deterministic equality index over authoritative collection state.
//!
//! PR33 attributed sustained-workload degradation to bounded query execution.
//! PR34 removed the whole-collection clone that dominated it, leaving execution
//! at `O(N)` because the conjunction still had to be evaluated against every
//! record of the collection. This module removes that remaining scan for the
//! queries it can answer, and *only* for those.
//!
//! The shape is deliberately narrow:
//!
//! ```text
//! collection -> field -> canonical value -> { record key, ... }
//! ```
//!
//! Three properties make this an execution structure rather than a second
//! database, and every one of them is load-bearing:
//!
//! 1. **It is derived.** The authoritative record map is the source of truth.
//!    Nothing here is durable, replicated, or consulted for anything but
//!    *candidate selection*. [`EqualityIndex::rebuild`] reconstructs the entire
//!    index from authoritative state, and a restart does exactly that.
//! 2. **It never decides a query.** A lookup returns record keys a query still
//!    has to evaluate its full predicate against. The index narrows what is
//!    examined; it does not define what matches.
//! 3. **It is maintained inside the state mutation boundary.** Every write goes
//!    through [`crate::Inner::put_row`] / [`crate::Inner::remove_row`], which
//!    hold the state lock across both the record change and the index change,
//!    so no externally observable committed state has one without the other.
//!
//! # Canonical values
//!
//! An index key must collide exactly when [`serde_json::Value`] equality holds,
//! because `serde_json::Value` equality is what the existing predicate applies.
//! [`IndexKey::of`] is therefore written against `serde_json`'s own equality
//! rules rather than against a serialization: `42`, `"42"`, `true`, `"true"`,
//! `null`, `0`, `"0"` and `false` all take distinct keys, and `42` and `42.0`
//! stay distinct because `serde_json` reports them unequal.
//!
//! PR35 indexes **scalar JSON values only**. Objects and arrays — and the
//! non-finite floats JSON cannot represent anyway — are *not indexable*, and a
//! record holding one contributes no entry for that field. That is safe rather
//! than lossy: an unindexable value can never equal an indexable one, so a
//! candidate set for a scalar condition is complete without it. A condition
//! whose expected value is unindexable makes the index inapplicable, and
//! execution falls back to the scan. Nothing is coerced to a string.

use crate::StoredRow;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The canonical, deterministic key one indexable JSON scalar maps to.
///
/// Determinism here is a correctness requirement, not a nicety: the same value
/// must produce the same key across a process lifetime, a restart, and a
/// rebuild, or a rebuilt index would answer differently from the live one.
/// Every variant is derived from the value's own contents — never from an
/// address, an iteration order, or a hash seed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndexKey {
    Null,
    Bool(bool),
    /// A `serde_json` positive integer, which is never equal to a float or a
    /// negative integer even when the arithmetic value would match.
    UnsignedInt(u64),
    /// A `serde_json` negative integer.
    SignedInt(i64),
    /// A finite float, keyed by its bit pattern with `-0.0` normalized to `0.0`
    /// because `-0.0 == 0.0` holds for the predicate too.
    Float(u64),
    Text(String),
}

impl IndexKey {
    /// The canonical key for a value, or `None` when the value is not indexable.
    ///
    /// `None` is not a failure: it is how this module refuses to invent a
    /// representation for something it cannot compare exactly.
    pub fn of(value: &Value) -> Option<Self> {
        match value {
            Value::Null => Some(Self::Null),
            Value::Bool(flag) => Some(Self::Bool(*flag)),
            Value::String(text) => Some(Self::Text(text.clone())),
            Value::Number(number) => {
                if let Some(unsigned) = number.as_u64() {
                    return Some(Self::UnsignedInt(unsigned));
                }
                if let Some(signed) = number.as_i64() {
                    return Some(Self::SignedInt(signed));
                }
                let float = number.as_f64()?;
                if !float.is_finite() {
                    return None;
                }
                // `-0.0 == 0.0` for the predicate, so the two must not take
                // different keys. Adding zero collapses the sign of zero and
                // leaves every other finite float untouched.
                Some(Self::Float((float + 0.0).to_bits()))
            }
            Value::Array(_) | Value::Object(_) => None,
        }
    }

    /// A stable textual rendering, for diagnostics and test comparison only.
    /// Nothing in execution depends on this form.
    pub fn describe(&self) -> String {
        match self {
            Self::Null => "null".to_string(),
            Self::Bool(flag) => format!("bool:{flag}"),
            Self::UnsignedInt(value) => format!("u64:{value}"),
            Self::SignedInt(value) => format!("i64:{value}"),
            Self::Float(bits) => format!("f64:{}", f64::from_bits(*bits)),
            Self::Text(text) => format!("str:{text}"),
        }
    }
}

/// The field identity the query surface exposes as the authority's record id.
///
/// It is deliberately never indexed. On the query surface `recordId` is
/// authority metadata that shadows a document field of the same name — but only
/// on object-valued records, a legacy asymmetry PR34 preserved verbatim. An
/// index over the document field would answer a different question from the one
/// the predicate asks, so a condition on this field is simply not index
/// eligible and falls back to the scan.
pub const RESERVED_RECORD_ID_FIELD: &str = "recordId";

/// Which fields of which collections are indexed, and the buckets for each.
///
/// Empty buckets are absent rather than retained: a value nothing holds has no
/// entry at all. That is the representation a rebuild produces, so it is the
/// representation live maintenance must converge to.
#[derive(Debug, Default, Clone)]
pub struct EqualityIndex {
    /// Declared indexes, as collection -> fields.
    declared: BTreeMap<String, BTreeSet<String>>,
    /// collection -> field -> canonical value -> record keys, in key order.
    ///
    /// Nested rather than keyed by a `(collection, field)` tuple so that
    /// maintenance can look a bucket up from `&str` without allocating. This is
    /// the write path of every indexed collection, so an allocation per mutated
    /// field would be a tax on inserts to buy nothing.
    buckets: HashMap<String, HashMap<String, ValueBuckets>>,
}

type ValueBuckets = HashMap<IndexKey, BTreeSet<String>>;

/// Add one record to one field's buckets, or do nothing when the record holds no
/// indexable value for that field.
///
/// A free function rather than a method so callers can hold a disjoint borrow of
/// `declared` while writing `buckets`.
fn insert_entry(
    buckets: &mut HashMap<String, HashMap<String, ValueBuckets>>,
    collection: &str,
    field: &str,
    key: &str,
    value: &Value,
) {
    let Some(index_key) = value.get(field).and_then(IndexKey::of) else {
        // A missing field and an unindexable value both contribute nothing.
        // They stay distinguishable from an explicit `null`, which is a value
        // and takes `IndexKey::Null`, because the predicate treats them as
        // distinct too.
        return;
    };
    let field_buckets = match buckets.get_mut(collection) {
        Some(existing) => existing,
        None => buckets.entry(collection.to_string()).or_default(),
    };
    let value_buckets = match field_buckets.get_mut(field) {
        Some(existing) => existing,
        None => field_buckets.entry(field.to_string()).or_default(),
    };
    value_buckets
        .entry(index_key)
        .or_default()
        .insert(key.to_string());
}

/// Remove one record from one field's buckets, dropping buckets that empty.
fn remove_entry(
    buckets: &mut HashMap<String, HashMap<String, ValueBuckets>>,
    collection: &str,
    field: &str,
    key: &str,
    value: &Value,
) {
    let Some(index_key) = value.get(field).and_then(IndexKey::of) else {
        return;
    };
    let Some(field_buckets) = buckets.get_mut(collection) else {
        return;
    };
    let Some(value_buckets) = field_buckets.get_mut(field) else {
        return;
    };
    if let Some(keys) = value_buckets.get_mut(&index_key) {
        keys.remove(key);
        if keys.is_empty() {
            value_buckets.remove(&index_key);
        }
    }
    if value_buckets.is_empty() {
        field_buckets.remove(field);
    }
    if field_buckets.is_empty() {
        buckets.remove(collection);
    }
}

impl EqualityIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every declared index, as `(collection, field)` pairs in a stable order.
    pub fn declarations(&self) -> Vec<(String, String)> {
        self.declared
            .iter()
            .flat_map(|(collection, fields)| {
                fields
                    .iter()
                    .map(move |field| (collection.clone(), field.clone()))
            })
            .collect()
    }

    /// Is this collection/field pair indexed?
    pub fn is_indexed(&self, collection: &str, field: &str) -> bool {
        self.declared
            .get(collection)
            .is_some_and(|fields| fields.contains(field))
    }

    /// Does this collection carry any index at all?
    ///
    /// Maintenance asks this first, so a collection nobody indexed costs one
    /// map lookup per mutation rather than a walk over its fields.
    pub fn indexes_collection(&self, collection: &str) -> bool {
        self.declared
            .get(collection)
            .is_some_and(|fields| !fields.is_empty())
    }

    /// Declare an index. Returns whether this call created it.
    ///
    /// Declaring is only half of `create`: the caller must populate the new
    /// index from authoritative state, which [`crate::FeltDb::create_equality_index`]
    /// does under the same lock. Declaring `recordId` is refused rather than
    /// silently ignored — see [`RESERVED_RECORD_ID_FIELD`].
    pub fn declare(&mut self, collection: &str, field: &str) -> Result<bool, String> {
        if collection.trim().is_empty() || field.trim().is_empty() {
            return Err("an equality index needs a collection and a field".to_string());
        }
        if field == RESERVED_RECORD_ID_FIELD {
            return Err(format!(
                "{RESERVED_RECORD_ID_FIELD} is authority metadata on the query surface and is not indexable"
            ));
        }
        Ok(self
            .declared
            .entry(collection.to_string())
            .or_default()
            .insert(field.to_string()))
    }

    /// Undeclare an index and drop its buckets. Returns whether one existed.
    pub fn undeclare(&mut self, collection: &str, field: &str) -> bool {
        let removed = self
            .declared
            .get_mut(collection)
            .is_some_and(|fields| fields.remove(field));
        if removed {
            if let Some(field_buckets) = self.buckets.get_mut(collection) {
                field_buckets.remove(field);
                if field_buckets.is_empty() {
                    self.buckets.remove(collection);
                }
            }
            if self
                .declared
                .get(collection)
                .is_some_and(|fields| fields.is_empty())
            {
                self.declared.remove(collection);
            }
        }
        removed
    }

    /// Forget every bucket, keeping declarations. The lifecycle's `clear`.
    pub fn clear(&mut self) {
        self.buckets.clear();
    }

    /// Forget declarations and buckets alike.
    pub fn reset(&mut self) {
        self.declared.clear();
        self.buckets.clear();
    }

    /// Add one record's entries for every declared field of its collection.
    ///
    /// `declared` and `buckets` are borrowed disjointly, so the write path walks
    /// the declared fields in place: no field name is cloned to mutate an index.
    pub fn insert_record(&mut self, collection: &str, key: &str, value: &Value) {
        let Self { declared, buckets } = self;
        let Some(fields) = declared.get(collection) else {
            return;
        };
        for field in fields {
            insert_entry(buckets, collection, field, key, value);
        }
    }

    /// Remove one record's entries for every declared field of its collection.
    pub fn remove_record(&mut self, collection: &str, key: &str, value: &Value) {
        let Self { declared, buckets } = self;
        let Some(fields) = declared.get(collection) else {
            return;
        };
        for field in fields {
            remove_entry(buckets, collection, field, key, value);
        }
    }

    /// Candidate record keys for one equality condition.
    ///
    /// `None` means *this index cannot answer that condition* — the field is not
    /// indexed, or the expected value is not indexable — and the caller must
    /// fall back. `Some(empty)` is an answer: no record holds that value.
    pub fn candidates(
        &self,
        collection: &str,
        field: &str,
        value: &Value,
    ) -> Option<&BTreeSet<String>> {
        if !self.is_indexed(collection, field) {
            return None;
        }
        let index_key = IndexKey::of(value)?;
        static EMPTY: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
        Some(
            self.buckets
                .get(collection)
                .and_then(|field_buckets| field_buckets.get(field))
                .and_then(|value_buckets| value_buckets.get(&index_key))
                .unwrap_or_else(|| EMPTY.get_or_init(BTreeSet::new)),
        )
    }

    /// Rebuild every declared index from authoritative state.
    ///
    /// This is the whole recovery story: an index is never loaded, replicated,
    /// or repaired — it is re-derived from the records that survived.
    pub fn rebuild(&mut self, rows: &HashMap<String, BTreeMap<String, StoredRow>>) {
        let Self { declared, buckets } = self;
        buckets.clear();
        for (collection, fields) in declared.iter() {
            let Some(bucket) = rows.get(collection) else {
                continue;
            };
            for field in fields {
                for (key, row) in bucket {
                    insert_entry(buckets, collection, field, key, &row.value);
                }
            }
        }
    }

    /// Rebuild the buckets of one declared field only.
    pub fn rebuild_field(
        &mut self,
        collection: &str,
        field: &str,
        rows: &HashMap<String, BTreeMap<String, StoredRow>>,
    ) {
        if let Some(field_buckets) = self.buckets.get_mut(collection) {
            field_buckets.remove(field);
        }
        let Some(bucket) = rows.get(collection) else {
            return;
        };
        for (key, row) in bucket {
            insert_entry(&mut self.buckets, collection, field, key, &row.value);
        }
    }

    /// A fully ordered rendering of the index, for tests and diagnostics.
    ///
    /// Comparing two of these is how the consistency invariant is asserted:
    /// the live index must equal one rebuilt from authoritative state. The form
    /// is deterministic — sorted collections, fields, keys and record ids — so
    /// equality of the rendering is equality of the index.
    pub fn snapshot(&self) -> BTreeMap<String, BTreeMap<String, BTreeMap<String, Vec<String>>>> {
        let mut snapshot: BTreeMap<String, BTreeMap<String, BTreeMap<String, Vec<String>>>> =
            BTreeMap::new();
        for (collection, field_buckets) in &self.buckets {
            for (field, value_buckets) in field_buckets {
                if value_buckets.is_empty() {
                    continue;
                }
                let entry = snapshot
                    .entry(collection.clone())
                    .or_default()
                    .entry(field.clone())
                    .or_default();
                for (index_key, keys) in value_buckets {
                    if keys.is_empty() {
                        continue;
                    }
                    entry.insert(index_key.describe(), keys.iter().cloned().collect());
                }
            }
        }
        snapshot
    }

    /// Aggregate shape of the index: declared indexes, buckets, and entries.
    /// Counts only — never a value and never a record id.
    pub fn stats(&self) -> EqualityIndexStats {
        let value_buckets = self.buckets.values().flat_map(HashMap::values);
        EqualityIndexStats {
            indexed_fields: self.declarations().len(),
            value_buckets: value_buckets.clone().map(HashMap::len).sum(),
            entries: value_buckets
                .flat_map(HashMap::values)
                .map(BTreeSet::len)
                .sum(),
        }
    }
}

/// Counts describing index size. Carries no record data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EqualityIndexStats {
    pub indexed_fields: usize,
    pub value_buckets: usize,
    pub entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(key: &str, value: Value) -> StoredRow {
        StoredRow {
            capability: "items".to_string(),
            key: key.to_string(),
            rust_type: "serde_json::Value".to_string(),
            value,
            unix_ms: 0,
            content_hash: None,
            flow_ref: None,
            deleted: false,
            operation: None,
        }
    }

    fn state(rows: Vec<StoredRow>) -> HashMap<String, BTreeMap<String, StoredRow>> {
        let mut state: HashMap<String, BTreeMap<String, StoredRow>> = HashMap::new();
        for row in rows {
            state
                .entry(row.capability.clone())
                .or_default()
                .insert(row.key.clone(), row);
        }
        state
    }

    #[test]
    fn distinguishable_scalars_never_collide() {
        let values = [
            json!(null),
            json!(true),
            json!(false),
            json!(0),
            json!(1),
            json!(-1),
            json!("0"),
            json!("1"),
            json!("true"),
            json!(42),
            json!("42"),
            json!(42.0),
            json!(""),
        ];
        let mut keys = Vec::new();
        for value in &values {
            keys.push(IndexKey::of(value).expect("scalar values are indexable"));
        }
        for (left_index, left) in values.iter().enumerate() {
            for (right_index, right) in values.iter().enumerate() {
                assert_eq!(
                    keys[left_index] == keys[right_index],
                    left == right,
                    "key equality must mirror value equality for {left} and {right}",
                );
            }
        }
    }

    #[test]
    fn signed_zero_shares_a_key_because_the_predicate_says_it_is_equal() {
        assert_eq!(json!(-0.0), json!(0.0));
        assert_eq!(IndexKey::of(&json!(-0.0)), IndexKey::of(&json!(0.0)));
    }

    #[test]
    fn compound_values_are_not_indexable() {
        assert_eq!(IndexKey::of(&json!({ "a": 1 })), None);
        assert_eq!(IndexKey::of(&json!([1, 2])), None);
    }

    #[test]
    fn record_id_cannot_be_declared() {
        let mut index = EqualityIndex::new();
        assert!(index.declare("items", RESERVED_RECORD_ID_FIELD).is_err());
        assert!(!index.is_indexed("items", RESERVED_RECORD_ID_FIELD));
    }

    #[test]
    fn maintenance_matches_a_rebuild() {
        let mut index = EqualityIndex::new();
        index.declare("items", "status").expect("declare");
        let rows = state(vec![
            row("items:a", json!({ "status": "active" })),
            row("items:b", json!({ "status": "active" })),
            row("items:c", json!({ "status": null })),
            row("items:d", json!({})),
            row("items:e", json!({ "status": { "nested": true } })),
            row("items:f", json!("not an object")),
        ]);
        for (key, stored) in &rows["items"] {
            index.insert_record("items", key, &stored.value);
        }

        let mut rebuilt = EqualityIndex::new();
        rebuilt.declare("items", "status").expect("declare");
        rebuilt.rebuild(&rows);
        assert_eq!(index.snapshot(), rebuilt.snapshot());

        let active = index
            .candidates("items", "status", &json!("active"))
            .expect("indexed field");
        assert_eq!(
            active.iter().cloned().collect::<Vec<_>>(),
            vec!["items:a".to_string(), "items:b".to_string()],
        );
        let null_bucket = index
            .candidates("items", "status", &json!(null))
            .expect("indexed field");
        assert_eq!(
            null_bucket.iter().cloned().collect::<Vec<_>>(),
            vec!["items:c".to_string()],
            "an explicit null is a value; a missing field is not",
        );
    }

    #[test]
    fn removing_the_last_holder_of_a_value_drops_the_bucket() {
        let mut index = EqualityIndex::new();
        index.declare("items", "status").expect("declare");
        let value = json!({ "status": "active" });
        index.insert_record("items", "items:a", &value);
        index.insert_record("items", "items:b", &value);
        index.remove_record("items", "items:a", &value);
        assert_eq!(index.stats().value_buckets, 1);
        index.remove_record("items", "items:b", &value);
        assert_eq!(
            index.stats(),
            EqualityIndexStats {
                indexed_fields: 1,
                value_buckets: 0,
                entries: 0
            },
            "an empty bucket is absent, which is what a rebuild would produce",
        );
        assert!(index.snapshot().is_empty());
    }

    #[test]
    fn an_unindexed_field_is_inapplicable_rather_than_empty() {
        let mut index = EqualityIndex::new();
        index.declare("items", "status").expect("declare");
        assert!(index.candidates("items", "tenant", &json!("a")).is_none());
        assert!(index
            .candidates("items", "status", &json!({ "compound": true }))
            .is_none());
        assert!(index
            .candidates("items", "status", &json!("nobody-holds-this"))
            .is_some_and(BTreeSet::is_empty));
    }
}
