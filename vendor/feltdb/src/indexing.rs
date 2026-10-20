use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IndexType {
    Hash,
    Sorted,
    Compound,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompoundKey {
    pub parts: Vec<String>,
}

impl CompoundKey {
    fn from_values(values: &[&str]) -> Self {
        CompoundKey {
            parts: values.iter().map(|v| v.to_string()).collect(),
        }
    }

    fn to_key(&self) -> String {
        self.parts.join("\0")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    pub name: String,
    pub index_type: IndexType,
    pub field: Option<String>,
    pub fields: Option<Vec<String>>,
    pub text_fields: Option<Vec<String>>,
    pub inmemory: Option<bool>,
    pub persistent: Option<bool>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    pub name: String,
    pub index_type: IndexType,
    pub entries: usize,
    pub size: usize,
    pub last_updated: u64,
    pub reads: u64,
    pub writes: u64,
    pub selectivity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlan {
    pub index_name: String,
    pub operator: String,
    pub selectivity: f64,
    pub estimated_rows: usize,
    pub cost: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginationParams {
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub ids: Vec<String>,
    pub total_count: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone)]
struct HashIndexInner {
    entries: HashMap<String, HashSet<String>>,
}

impl HashIndexInner {
    fn new() -> Self {
        HashIndexInner {
            entries: HashMap::new(),
        }
    }

    fn add(&mut self, record_id: &str, value: &str) {
        let key = value.to_string();
        self.entries
            .entry(key)
            .or_insert_with(HashSet::new)
            .insert(record_id.to_string());
    }

    fn remove(&mut self, record_id: &str, value: &str) {
        if let Some(ids) = self.entries.get_mut(value) {
            ids.remove(record_id);
            if ids.is_empty() {
                self.entries.remove(value);
            }
        }
    }

    fn query(&self, value: &str) -> Vec<String> {
        self.entries
            .get(value)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn size(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone)]
struct SortedIndexInner {
    entries: BTreeMap<String, HashSet<String>>,
}

impl SortedIndexInner {
    fn new() -> Self {
        SortedIndexInner {
            entries: BTreeMap::new(),
        }
    }

    fn add(&mut self, record_id: &str, value: &str) {
        self.entries
            .entry(value.to_string())
            .or_insert_with(HashSet::new)
            .insert(record_id.to_string());
    }

    fn remove(&mut self, record_id: &str, value: &str) {
        if let Some(ids) = self.entries.get_mut(value) {
            ids.remove(record_id);
            if ids.is_empty() {
                self.entries.remove(value);
            }
        }
    }

    fn query_equal(&self, value: &str) -> Vec<String> {
        self.entries
            .get(value)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn query_range(&self, min: &str, max: &str) -> Vec<String> {
        let mut result = HashSet::new();
        for (_key, ids) in self.entries.range(min.to_string()..=max.to_string()) {
            for id in ids {
                result.insert(id.clone());
            }
        }
        result.into_iter().collect()
    }

    fn size(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone)]
struct TextIndexInner {
    token_index: HashMap<String, HashSet<String>>,
}

impl TextIndexInner {
    fn new() -> Self {
        TextIndexInner {
            token_index: HashMap::new(),
        }
    }

    fn add(&mut self, record_id: &str, text: &str) {
        let tokens = Self::tokenize(text);
        for token in tokens {
            self.token_index
                .entry(token)
                .or_insert_with(HashSet::new)
                .insert(record_id.to_string());
        }
    }

    fn remove(&mut self, record_id: &str, text: &str) {
        let tokens = Self::tokenize(text);
        for token in tokens {
            if let Some(ids) = self.token_index.get_mut(&token) {
                ids.remove(record_id);
                if ids.is_empty() {
                    self.token_index.remove(&token);
                }
            }
        }
    }

    fn query(&self, search_text: &str) -> Vec<String> {
        let tokens = Self::tokenize(search_text);
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut result = self
            .token_index
            .get(&tokens[0])
            .cloned()
            .unwrap_or_default();

        for token in tokens.iter().skip(1) {
            if let Some(token_ids) = self.token_index.get(token) {
                result.retain(|id| token_ids.contains(id));
            } else {
                return Vec::new();
            }
        }

        result.into_iter().collect()
    }

    fn size(&self) -> usize {
        self.token_index.len()
    }

    fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split_whitespace()
            .map(|t| t.to_string())
            .collect()
    }
}

#[derive(Debug, Clone)]
struct CompoundIndexInner {
    entries: BTreeMap<String, HashSet<String>>,
    fields: Vec<String>,
}

impl CompoundIndexInner {
    fn new(fields: Vec<String>) -> Self {
        CompoundIndexInner {
            entries: BTreeMap::new(),
            fields,
        }
    }

    fn add(&mut self, record_id: &str, values: Vec<&str>) {
        let key = CompoundKey::from_values(&values).to_key();
        self.entries
            .entry(key)
            .or_insert_with(HashSet::new)
            .insert(record_id.to_string());
    }

    fn remove(&mut self, record_id: &str, values: Vec<&str>) {
        let key = CompoundKey::from_values(&values).to_key();
        if let Some(ids) = self.entries.get_mut(&key) {
            ids.remove(record_id);
            if ids.is_empty() {
                self.entries.remove(&key);
            }
        }
    }

    fn query_exact(&self, values: Vec<&str>) -> Vec<String> {
        let key = CompoundKey::from_values(&values).to_key();
        self.entries
            .get(&key)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn query_prefix(&self, prefix_values: Vec<&str>) -> Vec<String> {
        let prefix = CompoundKey::from_values(&prefix_values).to_key();
        let mut result = HashSet::new();
        for (key, ids) in self.entries.range(prefix.clone()..) {
            if !key.starts_with(&prefix) {
                break;
            }
            for id in ids {
                result.insert(id.clone());
            }
        }
        result.into_iter().collect()
    }

    fn size(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone)]
enum IndexInner {
    Hash(HashIndexInner),
    Sorted(SortedIndexInner),
    Compound(CompoundIndexInner),
    Text(TextIndexInner),
}

#[derive(Debug, Clone)]
struct IndexEntry {
    inner: IndexInner,
    config: IndexConfig,
    reads: u64,
    writes: u64,
}

pub struct IndexManager {
    indexes: HashMap<String, IndexEntry>,
}

impl IndexManager {
    pub fn new() -> Self {
        IndexManager {
            indexes: HashMap::new(),
        }
    }

    pub fn create_index(&mut self, config: IndexConfig) -> Result<(), String> {
        if self.indexes.contains_key(&config.name) {
            return Err(format!("Index {} already exists", config.name));
        }

        let inner = match config.index_type {
            IndexType::Hash => IndexInner::Hash(HashIndexInner::new()),
            IndexType::Sorted => IndexInner::Sorted(SortedIndexInner::new()),
            IndexType::Compound => {
                if let Some(fields) = &config.fields {
                    IndexInner::Compound(CompoundIndexInner::new(fields.clone()))
                } else {
                    return Err("Compound index requires 'fields' configuration".to_string());
                }
            }
            IndexType::Text => IndexInner::Text(TextIndexInner::new()),
        };

        self.indexes.insert(
            config.name.clone(),
            IndexEntry {
                inner,
                config,
                reads: 0,
                writes: 0,
            },
        );

        Ok(())
    }

    pub fn update_record(
        &mut self,
        record_id: &str,
        old_value: Option<&str>,
        new_value: &str,
    ) -> Result<(), String> {
        // Parse new_value as JSON to extract fields for compound indexes
        let new_obj: Option<serde_json::Value> = serde_json::from_str(new_value).ok();
        let old_obj: Option<serde_json::Value> =
            old_value.and_then(|v| serde_json::from_str(v).ok());

        for entry in self.indexes.values_mut() {
            if let Some(old) = old_value {
                match &mut entry.inner {
                    IndexInner::Hash(idx) => idx.remove(record_id, old),
                    IndexInner::Sorted(idx) => idx.remove(record_id, old),
                    IndexInner::Compound(idx) => {
                        if let Some(ref obj) = old_obj {
                            let values: Vec<&str> = idx
                                .fields
                                .iter()
                                .filter_map(|field| obj.get(field).and_then(|v| v.as_str()))
                                .collect();
                            if values.len() == idx.fields.len() {
                                idx.remove(record_id, values);
                            }
                        }
                    }
                    IndexInner::Text(idx) => idx.remove(record_id, old),
                }
            }

            match &mut entry.inner {
                IndexInner::Hash(idx) => idx.add(record_id, new_value),
                IndexInner::Sorted(idx) => idx.add(record_id, new_value),
                IndexInner::Compound(idx) => {
                    if let Some(ref obj) = new_obj {
                        let values: Vec<&str> = idx
                            .fields
                            .iter()
                            .filter_map(|field| obj.get(field).and_then(|v| v.as_str()))
                            .collect();
                        if values.len() == idx.fields.len() {
                            idx.add(record_id, values);
                        }
                    }
                }
                IndexInner::Text(idx) => idx.add(record_id, new_value),
            }

            entry.writes += 1;
        }

        Ok(())
    }

    pub fn query_index(
        &mut self,
        index_name: &str,
        operator: &str,
        value: &str,
    ) -> Result<Vec<String>, String> {
        let entry = self
            .indexes
            .get_mut(index_name)
            .ok_or_else(|| format!("Index {} not found", index_name))?;

        entry.reads += 1;

        let result = match &entry.inner {
            IndexInner::Hash(_) => {
                if operator == "eq" {
                    match &entry.inner {
                        IndexInner::Hash(idx) => idx.query(value),
                        _ => unreachable!(),
                    }
                } else {
                    return Err("Hash index only supports eq operator".to_string());
                }
            }
            IndexInner::Sorted(_) => {
                match operator {
                    "eq" => match &entry.inner {
                        IndexInner::Sorted(idx) => idx.query_equal(value),
                        _ => unreachable!(),
                    },
                    "range" => {
                        // For range, value should be "min,max"
                        let parts: Vec<&str> = value.split(',').collect();
                        if parts.len() != 2 {
                            return Err("Range query requires min,max format".to_string());
                        }
                        match &entry.inner {
                            IndexInner::Sorted(idx) => idx.query_range(parts[0], parts[1]),
                            _ => unreachable!(),
                        }
                    }
                    _ => {
                        return Err(format!(
                            "Sorted index does not support {} operator",
                            operator
                        ))
                    }
                }
            }
            IndexInner::Compound(_) => {
                match operator {
                    "eq" => {
                        // For exact match, value contains comma-separated field values
                        let values: Vec<&str> = value.split('\0').collect();
                        match &entry.inner {
                            IndexInner::Compound(idx) => idx.query_exact(values),
                            _ => unreachable!(),
                        }
                    }
                    "prefix" => {
                        // For prefix match (e.g., status=active,*)
                        let values: Vec<&str> = value.split('\0').collect();
                        match &entry.inner {
                            IndexInner::Compound(idx) => idx.query_prefix(values),
                            _ => unreachable!(),
                        }
                    }
                    _ => {
                        return Err(format!(
                            "Compound index does not support {} operator",
                            operator
                        ))
                    }
                }
            }
            IndexInner::Text(_) => {
                if operator == "contains" {
                    match &entry.inner {
                        IndexInner::Text(idx) => idx.query(value),
                        _ => unreachable!(),
                    }
                } else {
                    return Err("Text index only supports contains operator".to_string());
                }
            }
        };

        Ok(result)
    }

    pub fn list_indexes(&self) -> Vec<IndexConfig> {
        self.indexes.values().map(|e| e.config.clone()).collect()
    }

    pub fn get_stats(&self, name: &str) -> Option<IndexStats> {
        self.indexes.get(name).map(|e| IndexStats {
            name: e.config.name.clone(),
            index_type: e.config.index_type.clone(),
            entries: match &e.inner {
                IndexInner::Hash(idx) => idx.size(),
                IndexInner::Sorted(idx) => idx.size(),
                IndexInner::Compound(idx) => idx.size(),
                IndexInner::Text(idx) => idx.size(),
            },
            size: 0,         // TODO: implement size calculation
            last_updated: 0, // TODO: track timestamp
            reads: e.reads,
            writes: e.writes,
            selectivity: 0.5, // TODO: implement selectivity calculation
        })
    }
}

impl IndexManager {
    pub fn query_with_pagination(
        &mut self,
        index_name: &str,
        operator: &str,
        value: &str,
        params: PaginationParams,
    ) -> Result<QueryResult, String> {
        let all_ids = self.query_index(index_name, operator, value)?;
        let total_count = all_ids.len();

        let start = params.offset.min(total_count);
        let end = (params.offset + params.limit).min(total_count);

        let ids = all_ids[start..end].to_vec();
        let has_more = end < total_count;

        Ok(QueryResult {
            ids,
            total_count,
            limit: params.limit,
            offset: params.offset,
            has_more,
        })
    }

    pub fn plan_query(&self, index_name: &str, operator: &str) -> Result<QueryPlan, String> {
        let entry = self
            .indexes
            .get(index_name)
            .ok_or_else(|| format!("Index {} not found", index_name))?;

        let total_entries = match &entry.inner {
            IndexInner::Hash(idx) => idx.size(),
            IndexInner::Sorted(idx) => idx.size(),
            IndexInner::Compound(idx) => idx.size(),
            IndexInner::Text(idx) => idx.size(),
        };

        let selectivity = match operator {
            "eq" => 0.1,
            "range" => 0.3,
            "prefix" => 0.25,
            "contains" => 0.15,
            _ => 0.5,
        };

        let estimated_rows = ((total_entries as f64) * selectivity).ceil() as usize;
        let cost = (1.0 / selectivity).ceil() as u64;

        Ok(QueryPlan {
            index_name: index_name.to_string(),
            operator: operator.to_string(),
            selectivity,
            estimated_rows,
            cost,
        })
    }

    pub fn count_indexed(
        &mut self,
        index_name: &str,
        operator: &str,
        value: &str,
    ) -> Result<usize, String> {
        let results = self.query_index(index_name, operator, value)?;
        Ok(results.len())
    }

    pub fn exists_indexed(
        &mut self,
        index_name: &str,
        operator: &str,
        value: &str,
    ) -> Result<bool, String> {
        let count = self.count_indexed(index_name, operator, value)?;
        Ok(count > 0)
    }
}

impl Default for IndexManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_index() {
        let mut manager = IndexManager::new();
        manager
            .create_index(IndexConfig {
                name: "status_idx".to_string(),
                index_type: IndexType::Hash,
                field: Some("status".to_string()),
                fields: None,
                text_fields: None,
                inmemory: Some(true),
                persistent: Some(false),
                description: None,
            })
            .unwrap();

        manager.update_record("1", None, "active").unwrap();
        manager.update_record("2", None, "inactive").unwrap();
        manager.update_record("3", None, "active").unwrap();

        let results = manager.query_index("status_idx", "eq", "active").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_sorted_index() {
        let mut manager = IndexManager::new();
        manager
            .create_index(IndexConfig {
                name: "created_idx".to_string(),
                index_type: IndexType::Sorted,
                field: Some("created_at".to_string()),
                fields: None,
                text_fields: None,
                inmemory: Some(true),
                persistent: Some(false),
                description: None,
            })
            .unwrap();

        manager.update_record("1", None, "2024-01-01").unwrap();
        manager.update_record("2", None, "2024-02-01").unwrap();
        manager.update_record("3", None, "2024-03-01").unwrap();

        let results = manager
            .query_index("created_idx", "range", "2024-01-01,2024-02-01")
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_pagination() {
        let mut manager = IndexManager::new();
        manager
            .create_index(IndexConfig {
                name: "status_idx".to_string(),
                index_type: IndexType::Hash,
                field: Some("status".to_string()),
                fields: None,
                text_fields: None,
                inmemory: Some(true),
                persistent: Some(false),
                description: None,
            })
            .unwrap();

        for i in 1..=10 {
            manager
                .update_record(&i.to_string(), None, "active")
                .unwrap();
        }

        let result = manager
            .query_with_pagination(
                "status_idx",
                "eq",
                "active",
                PaginationParams {
                    limit: 3,
                    offset: 0,
                },
            )
            .unwrap();

        assert_eq!(result.ids.len(), 3);
        assert_eq!(result.total_count, 10);
        assert!(result.has_more);
    }

    #[test]
    fn test_count_indexed() {
        let mut manager = IndexManager::new();
        manager
            .create_index(IndexConfig {
                name: "status_idx".to_string(),
                index_type: IndexType::Hash,
                field: Some("status".to_string()),
                fields: None,
                text_fields: None,
                inmemory: Some(true),
                persistent: Some(false),
                description: None,
            })
            .unwrap();

        for i in 1..=5 {
            manager
                .update_record(&i.to_string(), None, "active")
                .unwrap();
        }

        let count = manager.count_indexed("status_idx", "eq", "active").unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn test_exists_indexed() {
        let mut manager = IndexManager::new();
        manager
            .create_index(IndexConfig {
                name: "status_idx".to_string(),
                index_type: IndexType::Hash,
                field: Some("status".to_string()),
                fields: None,
                text_fields: None,
                inmemory: Some(true),
                persistent: Some(false),
                description: None,
            })
            .unwrap();

        manager.update_record("1", None, "active").unwrap();

        assert!(manager
            .exists_indexed("status_idx", "eq", "active")
            .unwrap());
        assert!(!manager
            .exists_indexed("status_idx", "eq", "inactive")
            .unwrap());
    }

    #[test]
    fn test_query_plan() {
        let mut manager = IndexManager::new();
        manager
            .create_index(IndexConfig {
                name: "status_idx".to_string(),
                index_type: IndexType::Hash,
                field: Some("status".to_string()),
                fields: None,
                text_fields: None,
                inmemory: Some(true),
                persistent: Some(false),
                description: None,
            })
            .unwrap();

        let plan = manager.plan_query("status_idx", "eq").unwrap();
        assert_eq!(plan.index_name, "status_idx");
        assert_eq!(plan.operator, "eq");
        assert!(plan.selectivity > 0.0);
        assert!(plan.cost > 0);
    }
}
