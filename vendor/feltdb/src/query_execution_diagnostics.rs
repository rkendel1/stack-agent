//! Test instrumentation for the collection read paths a bounded query can take.
//!
//! PR33 attributed sustained-workload degradation to bounded queries cloning an
//! entire collection under the state mutex before predicate, ordering and limit
//! were applied. PR34 removes that clone, and a benchmark alone cannot prove a
//! clone is gone: a faster number is evidence of speed, not of mechanism.
//!
//! These counters therefore record *which traversal a read used*, so a test can
//! assert that a bounded query never entered `FeltDb::list_collection`. They
//! are process-global, monotonic, carry no record data, and are not part of the
//! query contract: nothing in the query path reads them, and no query result
//! depends on them. The HTTP surface only exposes them when a server is started
//! with `FELTDB_QUERY_DIAGNOSTICS=1`.
//!
//! PR35 extends them with execution attribution. An index is an optimization, so
//! "the query got faster" is not evidence that it was used; these counters say
//! which execution a query actually took, how many candidates the index
//! produced, and how many records the predicate had to evaluate. That last
//! figure is the one the PR35 claim rests on: for a selective indexed query it
//! must track the candidate count rather than the collection size.

use std::sync::atomic::{AtomicU64, Ordering};

static FULL_COLLECTION_MATERIALIZATIONS: AtomicU64 = AtomicU64::new(0);
static FULL_COLLECTION_RECORDS_CLONED: AtomicU64 = AtomicU64::new(0);
static COLLECTION_SCANS: AtomicU64 = AtomicU64::new(0);
static SCAN_RECORDS_VISITED: AtomicU64 = AtomicU64::new(0);
static SCAN_RECORDS_MATERIALIZED: AtomicU64 = AtomicU64::new(0);
static QUERIES_TOTAL: AtomicU64 = AtomicU64::new(0);
static QUERIES_SCAN: AtomicU64 = AtomicU64::new(0);
static QUERIES_INDEXED: AtomicU64 = AtomicU64::new(0);
static INDEX_HITS: AtomicU64 = AtomicU64::new(0);
static INDEX_MISSES: AtomicU64 = AtomicU64::new(0);
static INDEX_CANDIDATES_EXAMINED: AtomicU64 = AtomicU64::new(0);
static RECORDS_PREDICATE_EVALUATED: AtomicU64 = AtomicU64::new(0);

/// A point-in-time reading of the collection read counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryExecutionCounters {
    /// Calls to `FeltDb::list_collection`, which clones a whole collection.
    pub full_collection_materializations: u64,
    /// Records cloned by those calls.
    pub full_collection_records_cloned: u64,
    /// Calls to `FeltDb::query_collection`, which evaluates borrowed records.
    pub collection_scans: u64,
    /// Records a scan predicate observed, borrowed rather than cloned.
    pub scan_records_visited: u64,
    /// Records a scan actually cloned, which is at most the matching set.
    pub scan_records_materialized: u64,
    /// Collection reads that executed a query: `queries_scan + queries_indexed`.
    pub queries_total: u64,
    /// Queries answered by evaluating the predicate over the whole collection.
    pub queries_scan: u64,
    /// Queries answered from equality-index candidates.
    pub queries_indexed: u64,
    /// Queries for which at least one condition had an applicable index.
    pub index_hits: u64,
    /// Queries for which no condition had an applicable index, which fall back.
    pub index_misses: u64,
    /// Candidate record keys the index produced, after intersection.
    pub index_candidates_examined: u64,
    /// Records the predicate was actually evaluated against, on either path.
    /// For a selective indexed query this tracks the candidate count; for a scan
    /// it is the collection size.
    pub records_predicate_evaluated: u64,
}

pub(crate) fn record_full_materialization(records: usize) {
    FULL_COLLECTION_MATERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
    FULL_COLLECTION_RECORDS_CLONED.fetch_add(records as u64, Ordering::Relaxed);
}

pub(crate) fn record_scan(visited: usize, materialized: usize) {
    COLLECTION_SCANS.fetch_add(1, Ordering::Relaxed);
    SCAN_RECORDS_VISITED.fetch_add(visited as u64, Ordering::Relaxed);
    SCAN_RECORDS_MATERIALIZED.fetch_add(materialized as u64, Ordering::Relaxed);
    QUERIES_TOTAL.fetch_add(1, Ordering::Relaxed);
    QUERIES_SCAN.fetch_add(1, Ordering::Relaxed);
    // A scan evaluates the predicate against every record it visits. This is
    // the O(N) figure PR35 exists to reduce, recorded on the same counter the
    // indexed path reports to, so the two are directly comparable.
    RECORDS_PREDICATE_EVALUATED.fetch_add(visited as u64, Ordering::Relaxed);
}

/// A query for which no condition had an applicable index, so it falls back.
///
/// This deliberately does not advance `queries_total`: the scan the caller is
/// about to run records the query itself, and counting it here would report
/// every fallback twice.
pub(crate) fn record_index_miss() {
    INDEX_MISSES.fetch_add(1, Ordering::Relaxed);
}

/// A query answered from index candidates.
///
/// `examined` is the candidate set after intersection, `evaluated` the records
/// the predicate actually ran against, and `materialized` the matching set that
/// was cloned. `evaluated` can be below `examined` only if an index entry has no
/// record behind it, which would be a maintenance defect; the gap is visible
/// here rather than hidden.
pub(crate) fn record_indexed_query(examined: usize, evaluated: usize, materialized: usize) {
    QUERIES_TOTAL.fetch_add(1, Ordering::Relaxed);
    QUERIES_INDEXED.fetch_add(1, Ordering::Relaxed);
    INDEX_HITS.fetch_add(1, Ordering::Relaxed);
    INDEX_CANDIDATES_EXAMINED.fetch_add(examined as u64, Ordering::Relaxed);
    RECORDS_PREDICATE_EVALUATED.fetch_add(evaluated as u64, Ordering::Relaxed);
    SCAN_RECORDS_MATERIALIZED.fetch_add(materialized as u64, Ordering::Relaxed);
}

/// Read the counters. Monotonic since process start, so tests compare deltas.
pub fn counters() -> QueryExecutionCounters {
    QueryExecutionCounters {
        full_collection_materializations: FULL_COLLECTION_MATERIALIZATIONS.load(Ordering::Relaxed),
        full_collection_records_cloned: FULL_COLLECTION_RECORDS_CLONED.load(Ordering::Relaxed),
        collection_scans: COLLECTION_SCANS.load(Ordering::Relaxed),
        scan_records_visited: SCAN_RECORDS_VISITED.load(Ordering::Relaxed),
        scan_records_materialized: SCAN_RECORDS_MATERIALIZED.load(Ordering::Relaxed),
        queries_total: QUERIES_TOTAL.load(Ordering::Relaxed),
        queries_scan: QUERIES_SCAN.load(Ordering::Relaxed),
        queries_indexed: QUERIES_INDEXED.load(Ordering::Relaxed),
        index_hits: INDEX_HITS.load(Ordering::Relaxed),
        index_misses: INDEX_MISSES.load(Ordering::Relaxed),
        index_candidates_examined: INDEX_CANDIDATES_EXAMINED.load(Ordering::Relaxed),
        records_predicate_evaluated: RECORDS_PREDICATE_EVALUATED.load(Ordering::Relaxed),
    }
}
