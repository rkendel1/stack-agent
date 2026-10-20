use crate::{
    reconcile, resolve_path, ConflictClassification, MaterializationProjection, PathComponent, StateId,
    StateRevision,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub trait DurableObjectStore: Send + Sync + Clone + 'static {
    fn put_if_absent(&self, key: &str, value: &[u8]) -> Result<(), String>;
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    fn delete(&self, key: &str) -> Result<bool, String>;
    fn list_prefix(&self, prefix: &str) -> Result<Vec<String>, String>;
    fn compare_and_swap(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        replacement: &[u8],
    ) -> Result<bool, String>;
}

#[derive(Clone)]
pub struct LocalObjectStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl LocalObjectStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        Ok(Self {
            root: Arc::new(root),
            lock: Arc::new(Mutex::new(())),
        })
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(encode_key(key))
    }
}

impl DurableObjectStore for LocalObjectStore {
    fn put_if_absent(&self, key: &str, value: &[u8]) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(key);
        if path.exists() {
            return Ok(());
        }
        let tmp = self
            .root
            .join(format!(".tmp-{}-{}", std::process::id(), encode_key(key)));
        fs::write(&tmp, value).map_err(|e| e.to_string())?;
        match fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(_) if path.exists() => {
                let _ = fs::remove_file(&tmp);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path_for(key);
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn delete(&self, key: &str) -> Result<bool, String> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(key);
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    fn list_prefix(&self, prefix: &str) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(&*self.root).map_err(|e| e.to_string())?;
        let mut keys = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let Some(name) = entry.file_name().to_str().map(|v| v.to_string()) else {
                continue;
            };
            let key = decode_key(&name);
            if key.starts_with(prefix) {
                keys.push(key);
            }
        }
        keys.sort();
        Ok(keys)
    }

    fn compare_and_swap(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        replacement: &[u8],
    ) -> Result<bool, String> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(key);
        let current = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };

        let expected_matches = match (expected, current.as_deref()) {
            (None, None) => true,
            (Some(expected), Some(current)) => expected == current,
            _ => false,
        };
        if !expected_matches {
            return Ok(false);
        }

        let tmp = self
            .root
            .join(format!(".tmp-cas-{}-{}", std::process::id(), encode_key(key)));
        fs::write(&tmp, replacement).map_err(|e| e.to_string())?;
        fs::rename(tmp, path).map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[derive(Clone, Default)]
pub struct S3CompatibleBucket {
    inner: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

#[derive(Clone)]
pub struct S3CompatibleObjectStore {
    bucket: S3CompatibleBucket,
}

impl S3CompatibleObjectStore {
    pub fn new(bucket: S3CompatibleBucket) -> Self {
        Self { bucket }
    }
}

impl DurableObjectStore for S3CompatibleObjectStore {
    fn put_if_absent(&self, key: &str, value: &[u8]) -> Result<(), String> {
        self.bucket
            .inner
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_insert_with(|| value.to_vec());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.bucket.inner.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<bool, String> {
        Ok(self.bucket.inner.lock().unwrap().remove(key).is_some())
    }

    fn list_prefix(&self, prefix: &str) -> Result<Vec<String>, String> {
        let mut keys = self
            .bucket
            .inner
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        Ok(keys)
    }

    fn compare_and_swap(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        replacement: &[u8],
    ) -> Result<bool, String> {
        let mut guard = self.bucket.inner.lock().unwrap();
        let current = guard.get(key).map(|bytes| bytes.as_slice());
        let expected_matches = match (expected, current) {
            (None, None) => true,
            (Some(expected), Some(current)) => expected == current,
            _ => false,
        };
        if !expected_matches {
            return Ok(false);
        }
        guard.insert(key.to_string(), replacement.to_vec());
        Ok(true)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurableRef {
    pub namespace: String,
    pub name: String,
    pub target: StateId,
    pub version: u64,
    pub authority: String,
}

#[derive(Clone)]
pub struct VersionedObjectSubstrate<S: DurableObjectStore> {
    store: S,
    cache: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RevisionRecord {
    revision: StateRevision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubstrateError {
    NotFound(String),
    Conflict(String),
    Storage(String),
    Invalid(String),
}

impl std::fmt::Display for SubstrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubstrateError::NotFound(v) => write!(f, "{v}"),
            SubstrateError::Conflict(v) => write!(f, "{v}"),
            SubstrateError::Storage(v) => write!(f, "{v}"),
            SubstrateError::Invalid(v) => write!(f, "{v}"),
        }
    }
}

impl std::error::Error for SubstrateError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reachability {
    pub revisions: Vec<StateId>,
    pub objects: Vec<StateId>,
}

impl<S: DurableObjectStore> VersionedObjectSubstrate<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn clear_cache(&self) {
        self.cache.lock().unwrap().clear();
    }

    pub fn store_object(&self, content: &str) -> Result<StateId, SubstrateError> {
        let id = StateId::compute(content);
        let key = object_key(&id);
        self.store
            .put_if_absent(&key, content.as_bytes())
            .map_err(SubstrateError::Storage)?;
        self.cache
            .lock()
            .unwrap()
            .insert(key, content.as_bytes().to_vec());
        Ok(id)
    }

    pub fn get_object(&self, id: &StateId) -> Result<String, SubstrateError> {
        let key = object_key(id);
        // durable backend is authoritative; cache is advisory only
        let Some(bytes) = self.store.get(&key).map_err(SubstrateError::Storage)? else {
            self.cache.lock().unwrap().remove(&key);
            return Err(SubstrateError::NotFound(format!("object {} not found", id.as_hex())));
        };
        self.cache.lock().unwrap().insert(key, bytes.clone());
        String::from_utf8(bytes)
            .map_err(|e| SubstrateError::Invalid(format!("object is not utf8: {e}")))
    }

    pub fn create_revision(
        &self,
        resource: &str,
        content: String,
        parent: Option<StateId>,
        authority: String,
    ) -> Result<StateRevision, SubstrateError> {
        let parent_revision = if let Some(parent_id) = parent {
            Some(self.get_revision(&parent_id)?)
        } else {
            None
        };
        let revision = match parent_revision {
            Some(parent_revision) => {
                if parent_revision.resource != resource {
                    return Err(SubstrateError::Invalid(
                        "parent belongs to another resource".to_string(),
                    ));
                }
                StateRevision::child(content, &parent_revision, authority)
            }
            None => StateRevision::initial(resource.to_string(), content, authority),
        };
        let _ = self.store_object(&revision.content)?;
        self.persist_revision(&revision)?;
        Ok(revision)
    }

    pub fn get_revision(&self, id: &StateId) -> Result<StateRevision, SubstrateError> {
        let Some(bytes) = self
            .store
            .get(&revision_key(id))
            .map_err(SubstrateError::Storage)?
        else {
            return Err(SubstrateError::NotFound(format!("revision {} not found", id)));
        };
        let record: RevisionRecord =
            serde_json::from_slice(&bytes).map_err(|e| SubstrateError::Invalid(e.to_string()))?;
        let _ = self.get_object(&record.revision.content_id)?;
        Ok(record.revision)
    }

    pub fn list_revisions(&self) -> Result<Vec<StateRevision>, SubstrateError> {
        let keys = self
            .store
            .list_prefix("revision:")
            .map_err(SubstrateError::Storage)?;
        let mut revisions = keys
            .into_iter()
            .map(|key| {
                self.store
                    .get(&key)
                    .map_err(SubstrateError::Storage)?
                    .ok_or_else(|| SubstrateError::NotFound(format!("missing revision key {key}")))
                    .and_then(|bytes| {
                        serde_json::from_slice::<RevisionRecord>(&bytes)
                            .map(|record| record.revision)
                            .map_err(|e| SubstrateError::Invalid(e.to_string()))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        revisions.sort_by(|one, two| one.id.as_hex().cmp(two.id.as_hex()));
        Ok(revisions)
    }

    pub fn create_ref(
        &self,
        namespace: &str,
        name: &str,
        target: StateId,
        authority: String,
    ) -> Result<DurableRef, SubstrateError> {
        let _ = self.get_revision(&target)?;
        let reference = DurableRef {
            namespace: namespace.to_string(),
            name: name.to_string(),
            target,
            version: 1,
            authority,
        };
        let bytes = serde_json::to_vec(&reference)
            .map_err(|e| SubstrateError::Invalid(e.to_string()))?;
        let inserted = self
            .store
            .compare_and_swap(&ref_key(namespace, name), None, &bytes)
            .map_err(SubstrateError::Storage)?;
        if !inserted {
            return Err(SubstrateError::Conflict("ref already exists".to_string()));
        }
        Ok(reference)
    }

    pub fn get_ref(&self, namespace: &str, name: &str) -> Result<Option<DurableRef>, SubstrateError> {
        let Some(bytes) = self
            .store
            .get(&ref_key(namespace, name))
            .map_err(SubstrateError::Storage)?
        else {
            return Ok(None);
        };
        let reference =
            serde_json::from_slice(&bytes).map_err(|e| SubstrateError::Invalid(e.to_string()))?;
        Ok(Some(reference))
    }

    pub fn cas_move_ref(
        &self,
        namespace: &str,
        name: &str,
        expected_version: u64,
        target: StateId,
        authority: String,
    ) -> Result<DurableRef, SubstrateError> {
        let _ = self.get_revision(&target)?;
        let Some(current) = self.get_ref(namespace, name)? else {
            return Err(SubstrateError::NotFound("ref not found".to_string()));
        };
        if current.version != expected_version {
            return Err(SubstrateError::Conflict(format!(
                "stale version {}, current {}",
                expected_version, current.version
            )));
        }

        let next = DurableRef {
            version: current.version + 1,
            target,
            authority,
            ..current.clone()
        };
        let current_bytes =
            serde_json::to_vec(&current).map_err(|e| SubstrateError::Invalid(e.to_string()))?;
        let next_bytes =
            serde_json::to_vec(&next).map_err(|e| SubstrateError::Invalid(e.to_string()))?;
        let applied = self
            .store
            .compare_and_swap(&ref_key(namespace, name), Some(&current_bytes), &next_bytes)
            .map_err(SubstrateError::Storage)?;
        if !applied {
            return Err(SubstrateError::Conflict("concurrent ref update".to_string()));
        }
        Ok(next)
    }

    pub fn delete_ref(
        &self,
        namespace: &str,
        name: &str,
        expected_version: Option<u64>,
    ) -> Result<(), SubstrateError> {
        if let Some(expected) = expected_version {
            let Some(reference) = self.get_ref(namespace, name)? else {
                return Err(SubstrateError::NotFound("ref not found".to_string()));
            };
            if reference.version != expected {
                return Err(SubstrateError::Conflict("stale ref delete".to_string()));
            }
        }
        let deleted = self
            .store
            .delete(&ref_key(namespace, name))
            .map_err(SubstrateError::Storage)?;
        if !deleted {
            return Err(SubstrateError::NotFound("ref not found".to_string()));
        }
        Ok(())
    }

    pub fn list_refs(&self, namespace: Option<&str>) -> Result<Vec<DurableRef>, SubstrateError> {
        let keys = self
            .store
            .list_prefix("ref:")
            .map_err(SubstrateError::Storage)?;
        let mut refs = Vec::new();
        for key in keys {
            let Some(bytes) = self.store.get(&key).map_err(SubstrateError::Storage)? else {
                continue;
            };
            let reference: DurableRef =
                serde_json::from_slice(&bytes).map_err(|e| SubstrateError::Invalid(e.to_string()))?;
            if namespace.is_none_or(|scope| scope == reference.namespace) {
                refs.push(reference);
            }
        }
        refs.sort_by(|one, two| {
            one.namespace
                .cmp(&two.namespace)
                .then_with(|| one.name.cmp(&two.name))
        });
        Ok(refs)
    }

    pub fn materialize(
        &self,
        revision_id: &StateId,
        projection: &MaterializationProjection,
    ) -> Result<Value, SubstrateError> {
        let revision = self.get_revision(revision_id)?;
        let source: Value = serde_json::from_str(&revision.content)
            .map_err(|e| SubstrateError::Invalid(format!("unparseable json: {e}")))?;

        if projection.is_full() {
            return Ok(source);
        }

        let mut paths = projection.paths.clone();
        paths.sort();
        paths.dedup();
        if paths
            .windows(2)
            .any(|pair| path_overlap(&pair[0], &pair[1]))
        {
            return Err(SubstrateError::Invalid(
                "overlapping projection paths".to_string(),
            ));
        }

        let mut projected = Value::Object(serde_json::Map::new());
        for path in paths {
            let value = resolve_path(&source, &path)
                .cloned()
                .ok_or_else(|| SubstrateError::NotFound(format!("path missing: {path:?}")))?;
            assign_projection(&mut projected, &path, value)?;
        }
        Ok(projected)
    }

    pub fn conflict_classification(
        &self,
        base: &StateId,
        left: &StateId,
        right: &StateId,
    ) -> Result<ConflictClassification, SubstrateError> {
        let base = self.get_revision(base)?;
        let left = self.get_revision(left)?;
        let right = self.get_revision(right)?;
        Ok(ConflictClassification::classify(&base, &left, &right))
    }

    pub fn reconcile(
        &self,
        base: &StateId,
        left: &StateId,
        right: &StateId,
        authority: &str,
    ) -> Result<StateRevision, SubstrateError> {
        let base_revision = self.get_revision(base)?;
        let left_revision = self.get_revision(left)?;
        let right_revision = self.get_revision(right)?;
        let plan = crate::ReconciliationPlan::new(
            left_revision.id.clone(),
            right_revision.id.clone(),
            base_revision.id.clone(),
            true,
        );
        let reconciled = reconcile(
            &base_revision,
            &left_revision,
            &right_revision,
            &plan,
            authority.to_string(),
        )
        .map_err(SubstrateError::Invalid)?;
        self.persist_revision(&reconciled.materialized_state)?;
        let _ = self.store_object(&reconciled.materialized_state.content)?;
        Ok(reconciled.materialized_state)
    }

    pub fn reachability(&self) -> Result<Reachability, SubstrateError> {
        let refs = self.list_refs(None)?;
        let mut revision_ids = BTreeSet::new();
        let mut object_ids = BTreeSet::new();
        let mut stack = refs.into_iter().map(|reference| reference.target).collect::<Vec<_>>();
        while let Some(revision_id) = stack.pop() {
            if !revision_ids.insert(revision_id.clone()) {
                continue;
            }
            let revision = self.get_revision(&revision_id)?;
            object_ids.insert(revision.content_id.clone());
            if let Some(parent) = revision.parent_id {
                stack.push(parent);
            }
        }
        Ok(Reachability {
            revisions: revision_ids.into_iter().collect(),
            objects: object_ids.into_iter().collect(),
        })
    }

    pub fn collect_unreachable(&self) -> Result<Reachability, SubstrateError> {
        let reachable = self.reachability()?;
        let keep_revisions: BTreeSet<_> = reachable.revisions.iter().cloned().collect();
        let keep_objects: BTreeSet<_> = reachable.objects.iter().cloned().collect();

        for revision in self.list_revisions()? {
            if !keep_revisions.contains(&revision.id) {
                self.store
                    .delete(&revision_key(&revision.id))
                    .map_err(SubstrateError::Storage)?;
            }
        }

        for key in self
            .store
            .list_prefix("object:")
            .map_err(SubstrateError::Storage)?
        {
            if let Some(id_hex) = key.strip_prefix("object:") {
                let id = StateId::from_hex(id_hex.to_string());
                if !keep_objects.contains(&id) {
                    self.store.delete(&key).map_err(SubstrateError::Storage)?;
                }
            }
        }
        self.reachability()
    }

    fn persist_revision(&self, revision: &StateRevision) -> Result<(), SubstrateError> {
        let record = RevisionRecord {
            revision: revision.clone(),
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|e| SubstrateError::Invalid(format!("serialize revision: {e}")))?;
        self.store
            .put_if_absent(&revision_key(&revision.id), &bytes)
            .map_err(SubstrateError::Storage)
    }
}

fn revision_key(id: &StateId) -> String {
    format!("revision:{}", id.as_hex())
}

fn object_key(id: &StateId) -> String {
    format!("object:{}", id.as_hex())
}

fn ref_key(namespace: &str, name: &str) -> String {
    format!("ref:{}:{}", encode_key(namespace), encode_key(name))
}

fn encode_key(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

fn decode_key(value: &str) -> String {
    value
        .replace("%2F", "/")
        .replace("%3A", ":")
        .replace("%25", "%")
}

fn path_overlap(one: &[PathComponent], two: &[PathComponent]) -> bool {
    one == two
        || (one.len() < two.len() && two.starts_with(one))
        || (two.len() < one.len() && one.starts_with(two))
}

fn assign_projection(state: &mut Value, path: &[PathComponent], value: Value) -> Result<(), SubstrateError> {
    let Some((last, prefix)) = path.split_last() else {
        *state = value;
        return Ok(());
    };

    let mut current = state;
    for component in prefix {
        match component {
            PathComponent::Key(key) => {
                if current.is_null() {
                    *current = Value::Object(serde_json::Map::new());
                }
                let Some(object) = current.as_object_mut() else {
                    return Err(SubstrateError::Invalid("projection path type mismatch".to_string()));
                };
                current = object.entry(key.clone()).or_insert(Value::Null);
            }
            PathComponent::Index(index) => {
                if current.is_null() {
                    *current = Value::Array(Vec::new());
                }
                let Some(array) = current.as_array_mut() else {
                    return Err(SubstrateError::Invalid("projection path type mismatch".to_string()));
                };
                while array.len() <= *index {
                    array.push(Value::Null);
                }
                current = &mut array[*index];
            }
        }
    }

    match last {
        PathComponent::Key(key) => {
            if current.is_null() {
                *current = Value::Object(serde_json::Map::new());
            }
            let Some(object) = current.as_object_mut() else {
                return Err(SubstrateError::Invalid("projection terminal mismatch".to_string()));
            };
            object.insert(key.clone(), value);
            Ok(())
        }
        PathComponent::Index(index) => {
            if current.is_null() {
                *current = Value::Array(Vec::new());
            }
            let Some(array) = current.as_array_mut() else {
                return Err(SubstrateError::Invalid("projection terminal mismatch".to_string()));
            };
            while array.len() <= *index {
                array.push(Value::Null);
            }
            array[*index] = value;
            Ok(())
        }
    }
}
