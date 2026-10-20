use crate::Operation;
/// Trigger Engine module for event-driven execution
///
/// Triggers map operations to execution events based on:
/// - Event type (e.g., OrderCreated when Order is inserted)
/// - Optional filter conditions
/// - Capability to execute
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A trigger that maps operations to capability executions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    /// Unique trigger identifier
    pub trigger_id: String,

    /// Capability to execute when trigger fires
    pub capability: String,

    /// Event type this trigger listens for (e.g., "OrderCreated", "ItemUpdated")
    pub event_type: String,

    /// Collection this trigger applies to
    pub collection: String,

    /// Optional filter predicate (serialized as a JSON value)
    /// Format: { "field": "total", "operator": ">", "value": 100 }
    pub filter: Option<TriggerFilter>,

    /// Whether this trigger is enabled
    pub enabled: bool,
}

/// Filter condition for triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerFilter {
    /// The field path to check (e.g., "total", "user.role")
    pub field: String,

    /// The operator to use ("==", "!=", ">", "<", ">=", "<=", "contains", "in")
    pub operator: String,

    /// The value to compare against
    pub value: Value,
}

impl Trigger {
    /// Create a new trigger without filter
    pub fn new(
        trigger_id: String,
        capability: String,
        event_type: String,
        collection: String,
    ) -> Self {
        Self {
            trigger_id,
            capability,
            event_type,
            collection,
            filter: None,
            enabled: true,
        }
    }

    /// Create a new trigger with filter
    pub fn with_filter(
        trigger_id: String,
        capability: String,
        event_type: String,
        collection: String,
        filter: TriggerFilter,
    ) -> Self {
        Self {
            trigger_id,
            capability,
            event_type,
            collection,
            filter: Some(filter),
            enabled: true,
        }
    }

    /// Check if this trigger matches an operation
    pub fn matches(&self, operation: &Operation) -> bool {
        if !self.enabled {
            return false;
        }

        // Check if collection matches
        if operation.capability != self.collection {
            return false;
        }

        // Check if event type matches (for now, we map operation type to event type)
        // Insert -> event_type + "Created"
        // Update -> event_type + "Updated"
        // Delete -> event_type + "Deleted"
        let expected_event = match operation.op_type {
            crate::OperationType::Insert => {
                format!("{}Created", capitalize_singular(&self.collection))
            }
            crate::OperationType::Update => {
                format!("{}Updated", capitalize_singular(&self.collection))
            }
            crate::OperationType::Delete => {
                format!("{}Deleted", capitalize_singular(&self.collection))
            }
        };

        if expected_event != self.event_type {
            return false;
        }

        // Check filter if present
        if let Some(filter) = &self.filter {
            if let Some(value) = &operation.value {
                return filter.matches(value);
            } else {
                return false;
            }
        }

        true
    }
}

impl TriggerFilter {
    /// Check if this filter matches a value
    pub fn matches(&self, data: &Value) -> bool {
        let field_value = get_nested_value(data, &self.field);

        match field_value {
            None => false,
            Some(val) => match self.operator.as_str() {
                "==" => val == self.value,
                "!=" => val != self.value,
                ">" => compare_numeric(&val, &self.value, |a, b| a > b),
                "<" => compare_numeric(&val, &self.value, |a, b| a < b),
                ">=" => compare_numeric(&val, &self.value, |a, b| a >= b),
                "<=" => compare_numeric(&val, &self.value, |a, b| a <= b),
                "contains" => {
                    if let (Value::String(s), Value::String(needle)) = (val, &self.value) {
                        s.contains(needle.as_str())
                    } else {
                        false
                    }
                }
                "in" => {
                    if let Value::Array(arr) = &self.value {
                        arr.contains(&val)
                    } else {
                        false
                    }
                }
                _ => false,
            },
        }
    }
}

/// Trigger registry - manages all triggers
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TriggerRegistry {
    /// All triggers by trigger_id
    triggers: std::collections::HashMap<String, Trigger>,

    /// Triggers by collection
    triggers_by_collection: std::collections::HashMap<String, Vec<String>>,

    /// Triggers by event type
    triggers_by_event: std::collections::HashMap<String, Vec<String>>,
}

impl TriggerRegistry {
    /// Create a new trigger registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new trigger
    pub fn register(&mut self, trigger: Trigger) {
        let trigger_id = trigger.trigger_id.clone();
        let collection = trigger.collection.clone();
        let event_type = trigger.event_type.clone();

        self.triggers.insert(trigger_id.clone(), trigger);

        self.triggers_by_collection
            .entry(collection)
            .or_insert_with(Vec::new)
            .push(trigger_id.clone());

        self.triggers_by_event
            .entry(event_type)
            .or_insert_with(Vec::new)
            .push(trigger_id);
    }

    /// Unregister a trigger
    pub fn unregister(&mut self, trigger_id: &str) -> Option<Trigger> {
        if let Some(trigger) = self.triggers.remove(trigger_id) {
            // Remove from indexes
            if let Some(refs) = self.triggers_by_collection.get_mut(&trigger.collection) {
                refs.retain(|id| id != trigger_id);
            }
            if let Some(refs) = self.triggers_by_event.get_mut(&trigger.event_type) {
                refs.retain(|id| id != trigger_id);
            }
            Some(trigger)
        } else {
            None
        }
    }

    /// Get triggers that match an operation
    pub fn find_matching(&self, operation: &Operation) -> Vec<Trigger> {
        self.triggers_by_collection
            .get(&operation.capability)
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|id| self.triggers.get(id))
            .filter(|trigger| trigger.matches(operation))
            .cloned()
            .collect()
    }

    /// Get a trigger by ID
    pub fn get(&self, trigger_id: &str) -> Option<Trigger> {
        self.triggers.get(trigger_id).cloned()
    }

    /// Get all triggers
    pub fn all(&self) -> Vec<Trigger> {
        self.triggers.values().cloned().collect()
    }
}

// Helper functions

fn capitalize_singular(s: &str) -> String {
    // Convert "orders" -> "Order", "tasks" -> "Task"
    if s.ends_with('s') {
        let singular = &s[..s.len() - 1];
        format!(
            "{}{}",
            singular.chars().next().unwrap().to_uppercase(),
            &singular[1..]
        )
    } else {
        format!("{}{}", s.chars().next().unwrap().to_uppercase(), &s[1..])
    }
}

fn get_nested_value(obj: &Value, path: &str) -> Option<Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = obj;

    for part in parts {
        current = &current[part];
        if current.is_null() {
            return None;
        }
    }

    Some(current.clone())
}

fn compare_numeric<F>(a: &Value, b: &Value, cmp: F) -> bool
where
    F: Fn(f64, f64) -> bool,
{
    match (a.as_f64(), b.as_f64()) {
        (Some(av), Some(bv)) => cmp(av, bv),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_creation() {
        let trigger = Trigger::new(
            "trigger-1".to_string(),
            "process_order".to_string(),
            "OrderCreated".to_string(),
            "orders".to_string(),
        );

        assert_eq!(trigger.trigger_id, "trigger-1");
        assert_eq!(trigger.capability, "process_order");
        assert_eq!(trigger.enabled, true);
    }

    #[test]
    fn test_trigger_matching() {
        let trigger = Trigger::new(
            "trigger-1".to_string(),
            "process_order".to_string(),
            "OrderCreated".to_string(),
            "orders".to_string(),
        );

        let op = Operation::insert(
            1,
            "inst-a".to_string(),
            1,
            "orders:123".to_string(),
            serde_json::json!({"total": 100}),
            "Order".to_string(),
            "orders".to_string(),
        );

        assert!(trigger.matches(&op));
    }

    #[test]
    fn test_trigger_with_filter() {
        let filter = TriggerFilter {
            field: "total".to_string(),
            operator: ">".to_string(),
            value: serde_json::json!(100),
        };

        let trigger = Trigger::with_filter(
            "trigger-1".to_string(),
            "send_discount".to_string(),
            "OrderCreated".to_string(),
            "orders".to_string(),
            filter,
        );

        // Should match order with total > 100
        let op1 = Operation::insert(
            1,
            "inst-a".to_string(),
            1,
            "orders:123".to_string(),
            serde_json::json!({"total": 150}),
            "Order".to_string(),
            "orders".to_string(),
        );
        assert!(trigger.matches(&op1));

        // Should not match order with total <= 100
        let op2 = Operation::insert(
            2,
            "inst-a".to_string(),
            2,
            "orders:124".to_string(),
            serde_json::json!({"total": 50}),
            "Order".to_string(),
            "orders".to_string(),
        );
        assert!(!trigger.matches(&op2));
    }

    #[test]
    fn test_trigger_registry() {
        let mut registry = TriggerRegistry::new();

        let trigger1 = Trigger::new(
            "trigger-1".to_string(),
            "process_order".to_string(),
            "OrderCreated".to_string(),
            "orders".to_string(),
        );

        let trigger2 = Trigger::new(
            "trigger-2".to_string(),
            "send_notification".to_string(),
            "OrderUpdated".to_string(),
            "orders".to_string(),
        );

        registry.register(trigger1);
        registry.register(trigger2);

        assert_eq!(registry.all().len(), 2);
        assert!(registry.get("trigger-1").is_some());
    }

    #[test]
    fn test_capitalize_singular() {
        assert_eq!(capitalize_singular("orders"), "Order");
        assert_eq!(capitalize_singular("tasks"), "Task");
        assert_eq!(capitalize_singular("items"), "Item");
    }
}
