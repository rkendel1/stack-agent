//! Canonical resource authorization and portable signed grants.
//!
//! This module is deliberately independent of HTTP and Studio. Rust owns every
//! security decision; other surfaces serialize and consume these contracts.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const AUTHORIZATION_CONTRACT_VERSION: u32 = 1;
pub const POLICY_VERSION: &str = "feltdb.resource-authorization.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Subject {
    Human(String),
    Service(String),
    Workload(String),
    Agent(String),
    Connection(String),
    Worker(String),
}
impl Subject {
    pub fn id(&self) -> &str {
        match self {
            Self::Human(v)
            | Self::Service(v)
            | Self::Workload(v)
            | Self::Agent(v)
            | Self::Connection(v)
            | Self::Worker(v) => v,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Human(_) => "human",
            Self::Service(_) => "service",
            Self::Workload(_) => "workload",
            Self::Agent(_) => "agent",
            Self::Connection(_) => "connection",
            Self::Worker(_) => "worker",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resource {
    pub uri: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}
impl Resource {
    pub fn new(application_id: &str, path: &str) -> Result<Self, AuthorizationError> {
        let uri = format!("flow://{}/{}", application_id, path.trim_start_matches('/'));
        validate_resource_uri(&uri, application_id)?;
        Ok(Self {
            uri,
            attributes: BTreeMap::new(),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResourceScopeKind {
    Application,
    Collection,
    Resource,
    ResourcePrefix,
    Workload,
    Connection,
    Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceScope {
    pub kind: ResourceScopeKind,
    pub uri: String,
}
impl ResourceScope {
    pub fn matches(&self, resource: &Resource) -> bool {
        match self.kind {
            ResourceScopeKind::Application => {
                resource.uri == self.uri
                    || resource
                        .uri
                        .starts_with(&(self.uri.trim_end_matches('/').to_owned() + "/"))
            }
            ResourceScopeKind::ResourcePrefix => {
                let prefix = self.uri.trim_end_matches('*').trim_end_matches('/');
                resource.uri == prefix || resource.uri.starts_with(&(prefix.to_owned() + "/"))
            }
            _ => resource.uri == self.uri,
        }
    }
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        parent.matches(&Resource {
            uri: self.uri.trim_end_matches('*').trim_end_matches('/').into(),
            attributes: BTreeMap::new(),
        }) && (!matches!(self.kind, ResourceScopeKind::Application)
            || matches!(parent.kind, ResourceScopeKind::Application))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationContext {
    pub environment: Option<String>,
    pub execution_id: Option<String>,
    pub workload_id: Option<String>,
    pub worker_id: Option<String>,
    pub connection_id: Option<String>,
    pub timestamp: i64,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}
impl AuthorizationContext {
    pub fn now() -> Self {
        Self {
            timestamp: unix_seconds(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantConstraints {
    #[serde(default)]
    pub environments: BTreeSet<String>,
    pub worker_class: Option<String>,
    pub network: Option<String>,
    pub workload_id: Option<String>,
    pub connection_enabled: Option<bool>,
    pub minimum_schema_version: Option<u64>,
    #[serde(default)]
    pub subject_attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub resource_attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub context_attributes: BTreeMap<String, String>,
}
impl GrantConstraints {
    fn permits(
        &self,
        subject: &Subject,
        resource: &Resource,
        context: &AuthorizationContext,
    ) -> bool {
        (self.environments.is_empty()
            || context
                .environment
                .as_ref()
                .is_some_and(|v| self.environments.contains(v)))
            && self
                .workload_id
                .as_ref()
                .is_none_or(|v| context.workload_id.as_ref() == Some(v))
            && self
                .worker_class
                .as_ref()
                .is_none_or(|v| context.attributes.get("worker_class") == Some(v))
            && self
                .network
                .as_ref()
                .is_none_or(|v| context.attributes.get("network") == Some(v))
            && self.connection_enabled.is_none_or(|v| {
                context
                    .attributes
                    .get("connection_enabled")
                    .map(|x| x == "true")
                    == Some(v)
            })
            && self.minimum_schema_version.is_none_or(|v| {
                context
                    .attributes
                    .get("schema_version")
                    .and_then(|x| x.parse().ok())
                    .is_some_and(|x: u64| x >= v)
            })
            && self.subject_attributes.iter().all(|(k, v)| {
                (k == "type" && subject.kind() == v)
                    || context.attributes.get(&format!("subject.{k}")) == Some(v)
            })
            && self
                .resource_attributes
                .iter()
                .all(|(k, v)| resource.attributes.get(k) == Some(v))
            && self
                .context_attributes
                .iter()
                .all(|(k, v)| context.attributes.get(k) == Some(v))
    }
    fn is_subset_of(&self, parent: &Self) -> bool {
        (parent.environments.is_empty() || self.environments.is_subset(&parent.environments))
            && parent
                .worker_class
                .as_ref()
                .is_none_or(|v| self.worker_class.as_ref() == Some(v))
            && parent
                .network
                .as_ref()
                .is_none_or(|v| self.network.as_ref() == Some(v))
            && parent
                .workload_id
                .as_ref()
                .is_none_or(|v| self.workload_id.as_ref() == Some(v))
            && parent
                .connection_enabled
                .is_none_or(|v| self.connection_enabled == Some(v))
            && parent
                .minimum_schema_version
                .is_none_or(|v| self.minimum_schema_version.is_some_and(|child| child >= v))
            && parent
                .subject_attributes
                .iter()
                .all(|(k, v)| self.subject_attributes.get(k) == Some(v))
            && parent
                .resource_attributes
                .iter()
                .all(|(k, v)| self.resource_attributes.get(k) == Some(v))
            && parent
                .context_attributes
                .iter()
                .all(|(k, v)| self.context_attributes.get(k) == Some(v))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GrantStatus {
    Active,
    Revoked,
    Expired,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub contract_version: u32,
    pub id: String,
    pub issuer: Subject,
    pub subject: Subject,
    pub tenant_id: String,
    pub application_id: String,
    pub capabilities: BTreeSet<String>,
    pub resources: Vec<ResourceScope>,
    #[serde(default)]
    pub constraints: GrantConstraints,
    pub issued_at: i64,
    pub expires_at: i64,
    pub parent_grant: Option<String>,
    pub revision: u64,
    pub status: GrantStatus,
}
impl Grant {
    pub fn is_subset_of(&self, parent: &Grant) -> bool {
        self.tenant_id == parent.tenant_id
            && self.application_id == parent.application_id
            && self.issued_at >= parent.issued_at
            && self.expires_at <= parent.expires_at
            && self.capabilities.is_subset(&parent.capabilities)
            && self.resources.iter().all(|child| {
                parent
                    .resources
                    .iter()
                    .any(|scope| child.is_subset_of(scope))
            })
            && self.constraints.is_subset_of(&parent.constraints)
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, AuthorizationError> {
        serde_json::to_vec(self)
            .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))
    }
    pub fn digest(&self) -> Result<String, AuthorizationError> {
        Ok(hex(&Sha256::digest(self.canonical_bytes()?)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedGrant {
    pub algorithm: String,
    pub key_id: String,
    pub grant: Grant,
    pub digest: String,
    pub signature: String,
}

#[derive(Clone)]
pub struct GrantSigner {
    key_id: String,
    signing: Option<SigningKey>,
    verifying: VerifyingKey,
}
impl GrantSigner {
    pub fn new(
        key_id: impl Into<String>,
        key: impl Into<Vec<u8>>,
    ) -> Result<Self, AuthorizationError> {
        let key = key.into();
        if key.len() != 32 {
            return Err(AuthorizationError::new(
                "GRANT_INVALID",
                "Ed25519 signing key must be 32 bytes",
            ));
        }
        let bytes: [u8; 32] = key
            .try_into()
            .map_err(|_| AuthorizationError::new("GRANT_INVALID", "invalid signing key"))?;
        let signing = SigningKey::from_bytes(&bytes);
        let verifying = signing.verifying_key();
        Ok(Self {
            key_id: key_id.into(),
            signing: Some(signing),
            verifying,
        })
    }
    pub fn verifier(
        key_id: impl Into<String>,
        public_key: &[u8],
    ) -> Result<Self, AuthorizationError> {
        let bytes: [u8; 32] = public_key.try_into().map_err(|_| {
            AuthorizationError::new("GRANT_INVALID", "Ed25519 public key must be 32 bytes")
        })?;
        let verifying = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| AuthorizationError::new("GRANT_INVALID", "invalid Ed25519 public key"))?;
        Ok(Self {
            key_id: key_id.into(),
            signing: None,
            verifying,
        })
    }
    pub fn verifier_hex(
        key_id: impl Into<String>,
        public_key: &str,
    ) -> Result<Self, AuthorizationError> {
        Self::verifier(key_id, &decode_hex(public_key)?)
    }
    pub fn public_key_hex(&self) -> String {
        hex(self.verifying.as_bytes())
    }
    pub fn sign(&self, grant: Grant) -> Result<SignedGrant, AuthorizationError> {
        let bytes = grant.canonical_bytes()?;
        let digest = hex(&Sha256::digest(&bytes));
        let signing = self.signing.as_ref().ok_or_else(|| {
            AuthorizationError::new("GRANT_INVALID", "verification-only key cannot sign grants")
        })?;
        Ok(SignedGrant {
            algorithm: "ED25519-V1".into(),
            key_id: self.key_id.clone(),
            signature: hex(&signing.sign(&bytes).to_bytes()),
            digest,
            grant,
        })
    }
    pub fn verify_signature(&self, signed: &SignedGrant) -> Result<(), AuthorizationError> {
        if signed.algorithm != "ED25519-V1" || signed.key_id != self.key_id {
            return Err(AuthorizationError::new(
                "GRANT_INVALID",
                "unsupported signer",
            ));
        }
        let bytes = signed.grant.canonical_bytes()?;
        let expected_digest = hex(&Sha256::digest(&bytes));
        let signature = Signature::from_slice(&decode_hex(&signed.signature)?)
            .map_err(|_| AuthorizationError::new("GRANT_INVALID", "invalid Ed25519 signature"))?;
        if self.verifying.verify(&bytes, &signature).is_err() || expected_digest != signed.digest {
            return Err(AuthorizationError::new(
                "GRANT_INVALID",
                "grant signature or digest is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationRequest {
    pub subject: Subject,
    pub tenant_id: String,
    pub application_id: String,
    pub resource: Resource,
    pub capability: String,
    #[serde(default)]
    pub context: AuthorizationContext,
    pub grant_id: Option<String>,
    #[serde(default)]
    pub subject_attributes: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationDecision {
    pub decision_id: String,
    pub request_hash: String,
    pub decision_hash: String,
    pub allowed: bool,
    pub subject: Subject,
    pub tenant_id: String,
    pub application_id: String,
    pub resource: Resource,
    pub capability: String,
    pub grant_id: Option<String>,
    pub policy_version: String,
    pub evaluated_at: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizationError {
    pub code: String,
    pub message: String,
}
impl AuthorizationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AuthorizationError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RevocationState {
    #[serde(default)]
    pub grants: BTreeSet<String>,
    #[serde(default)]
    pub subjects: BTreeSet<Subject>,
    #[serde(default)]
    pub applications: BTreeSet<String>,
    #[serde(default)]
    pub tenants: BTreeSet<String>,
}

/// File-backed control-plane state. Every mutation is persisted before it is
/// returned to callers, so revocation and delegation survive process restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantStore {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    pub grants: BTreeMap<String, SignedGrant>,
    #[serde(default)]
    pub revocations: RevocationState,
    #[serde(default)]
    pub decisions: BTreeMap<String, AuthorizationDecision>,
}
impl GrantStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AuthorizationError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                grants: BTreeMap::new(),
                revocations: Default::default(),
                decisions: BTreeMap::new(),
            });
        }
        let mut store: Self = serde_json::from_slice(
            &fs::read(&path)
                .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))?,
        )
        .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))?;
        store.path = path;
        Ok(store)
    }
    fn persist(&self) -> Result<(), AuthorizationError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))?;
        }
        let temporary = self.path.with_extension("tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(self)
                .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))?,
        )
        .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))?;
        fs::rename(temporary, &self.path)
            .map_err(|e| AuthorizationError::new("GRANT_INVALID", e.to_string()))
    }
    pub fn create(
        &mut self,
        grant: Grant,
        signer: &GrantSigner,
    ) -> Result<SignedGrant, AuthorizationError> {
        if grant.contract_version != AUTHORIZATION_CONTRACT_VERSION
            || grant.expires_at <= grant.issued_at
            || grant.status != GrantStatus::Active
            || self.grants.contains_key(&grant.id)
        {
            return Err(AuthorizationError::new(
                "GRANT_INVALID",
                "invalid or duplicate grant lifecycle",
            ));
        }
        if grant.parent_grant.is_some() {
            return Err(AuthorizationError::new(
                "GRANT_DELEGATION_INVALID",
                "use delegate for a derived grant",
            ));
        }
        validate_scopes(&grant)?;
        let signed = signer.sign(grant)?;
        self.grants.insert(signed.grant.id.clone(), signed.clone());
        self.persist()?;
        Ok(signed)
    }
    pub fn delegate(
        &mut self,
        mut grant: Grant,
        parent_id: &str,
        signer: &GrantSigner,
    ) -> Result<SignedGrant, AuthorizationError> {
        let parent = self.grants.get(parent_id).ok_or_else(|| {
            AuthorizationError::new("GRANT_DELEGATION_INVALID", "parent grant not found")
        })?;
        if self.revocations.grants.contains(parent_id) || parent.grant.status != GrantStatus::Active
        {
            return Err(AuthorizationError::new(
                "GRANT_REVOKED",
                "parent grant is not active",
            ));
        }
        grant.parent_grant = Some(parent_id.into());
        if grant.issuer != parent.grant.subject {
            return Err(AuthorizationError::new(
                "GRANT_DELEGATION_INVALID",
                "derived grant issuer must be the parent subject",
            ));
        }
        if !grant.is_subset_of(&parent.grant) {
            return Err(AuthorizationError::new(
                "GRANT_DELEGATION_INVALID",
                "delegation broadens parent authority",
            ));
        }
        validate_scopes(&grant)?;
        let signed = signer.sign(grant)?;
        self.grants.insert(signed.grant.id.clone(), signed.clone());
        self.persist()?;
        Ok(signed)
    }
    pub fn revoke(
        &mut self,
        grant_id: &str,
        signer: &GrantSigner,
    ) -> Result<SignedGrant, AuthorizationError> {
        let signed = self
            .grants
            .get_mut(grant_id)
            .ok_or_else(|| AuthorizationError::new("GRANT_INVALID", "grant not found"))?;
        signed.grant.status = GrantStatus::Revoked;
        *signed = signer.sign(signed.grant.clone())?;
        self.revocations.grants.insert(grant_id.into());
        let result = signed.clone();
        self.persist()?;
        Ok(result)
    }
    pub fn check(
        &mut self,
        request: &AuthorizationRequest,
        signer: &GrantSigner,
    ) -> AuthorizationDecision {
        let signed = request.grant_id.as_ref().and_then(|id| self.grants.get(id));
        let decision = authorize(request, signed, signer, &self.grants, &self.revocations);
        self.decisions
            .insert(decision.decision_id.clone(), decision.clone());
        let _ = self.persist();
        decision
    }
}

fn validate_scopes(grant: &Grant) -> Result<(), AuthorizationError> {
    if grant.capabilities.is_empty() || grant.resources.is_empty() {
        return Err(AuthorizationError::new(
            "GRANT_INVALID",
            "grant requires capabilities and resource scopes",
        ));
    }
    for scope in &grant.resources {
        validate_resource_uri(
            scope.uri.trim_end_matches('*').trim_end_matches('/'),
            &grant.application_id,
        )?;
    }
    Ok(())
}

pub fn verify_grant(
    signed: &SignedGrant,
    signer: &GrantSigner,
    request: &AuthorizationRequest,
    grants: &BTreeMap<String, SignedGrant>,
    revoked: &RevocationState,
) -> Result<(), AuthorizationError> {
    signer.verify_signature(signed)?;
    let grant = &signed.grant;
    if grant.subject != request.subject {
        return Err(AuthorizationError::new(
            "GRANT_SCOPE_DENIED",
            "wrong subject",
        ));
    }
    if grant.tenant_id != request.tenant_id || grant.application_id != request.application_id {
        return Err(AuthorizationError::new(
            "GRANT_SCOPE_DENIED",
            "cross-tenant or cross-application grant",
        ));
    }
    validate_resource_uri(&request.resource.uri, &request.application_id)?;
    if grant.expires_at <= request.context.timestamp || grant.status == GrantStatus::Expired {
        return Err(AuthorizationError::new(
            "GRANT_EXPIRED",
            "grant has expired",
        ));
    }
    if grant.status == GrantStatus::Revoked
        || revoked.grants.contains(&grant.id)
        || revoked.subjects.contains(&grant.subject)
        || revoked.applications.contains(&grant.application_id)
        || revoked.tenants.contains(&grant.tenant_id)
    {
        return Err(AuthorizationError::new("GRANT_REVOKED", "grant is revoked"));
    }
    if !grant.capabilities.contains(&request.capability)
        || !grant
            .resources
            .iter()
            .any(|scope| scope.matches(&request.resource))
    {
        return Err(AuthorizationError::new(
            "GRANT_SCOPE_DENIED",
            "capability has no applicable resource scope",
        ));
    }
    if !grant
        .constraints
        .permits(&grant.subject, &request.resource, &request.context)
    {
        return Err(AuthorizationError::new(
            "GRANT_SCOPE_DENIED",
            "authorization context does not satisfy grant constraints",
        ));
    }
    if let Some(parent_id) = &grant.parent_grant {
        let parent = grants.get(parent_id).ok_or_else(|| {
            AuthorizationError::new("GRANT_DELEGATION_INVALID", "parent grant not found")
        })?;
        let parent_request = AuthorizationRequest {
            subject: parent.grant.subject.clone(),
            grant_id: Some(parent_id.clone()),
            ..request.clone()
        };
        verify_grant(parent, signer, &parent_request, grants, revoked)?;
        if !grant.is_subset_of(&parent.grant) {
            return Err(AuthorizationError::new(
                "GRANT_DELEGATION_INVALID",
                "derived grant broadens parent authority",
            ));
        }
    }
    Ok(())
}

pub fn authorize(
    request: &AuthorizationRequest,
    signed: Option<&SignedGrant>,
    signer: &GrantSigner,
    grants: &BTreeMap<String, SignedGrant>,
    revoked: &RevocationState,
) -> AuthorizationDecision {
    let evaluated_at = request.context.timestamp;
    let request_bytes = serde_json::to_vec(request).unwrap_or_default();
    let request_hash = hex(&Sha256::digest(request_bytes));
    let result = signed
        .ok_or_else(|| AuthorizationError::new("GRANT_SCOPE_DENIED", "a scoped grant is required"))
        .and_then(|grant| verify_grant(grant, signer, request, grants, revoked));
    let allowed = result.is_ok();
    let reason = result
        .err()
        .map(|e| format!("{}: {}", e.code, e.message))
        .unwrap_or_else(|| "GRANT_ALLOWED: signed grant authorizes resource operation".into());
    let seed = format!("{request_hash}:{evaluated_at}:{allowed}:{reason}");
    let decision_id = format!("decision_{}", &hex(&Sha256::digest(seed.as_bytes()))[..24]);
    let mut decision = AuthorizationDecision {
        decision_id,
        request_hash,
        decision_hash: String::new(),
        allowed,
        subject: request.subject.clone(),
        tenant_id: request.tenant_id.clone(),
        application_id: request.application_id.clone(),
        resource: request.resource.clone(),
        capability: request.capability.clone(),
        grant_id: signed.map(|v| v.grant.id.clone()),
        policy_version: POLICY_VERSION.into(),
        evaluated_at,
        reason,
    };
    decision.decision_hash = hex(&Sha256::digest(
        serde_json::to_vec(&decision).unwrap_or_default(),
    ));
    decision
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationRequest {
    pub original_grant: SignedGrant,
    pub current_request: AuthorizationRequest,
    pub original_revision: String,
    pub current_revision: String,
}
pub fn reauthorize_reconciliation(
    input: &ReconciliationRequest,
    signer: &GrantSigner,
    grants: &BTreeMap<String, SignedGrant>,
    revoked: &RevocationState,
) -> Result<AuthorizationDecision, AuthorizationError> {
    let decision = authorize(
        &input.current_request,
        Some(&input.original_grant),
        signer,
        grants,
        revoked,
    );
    if !decision.allowed {
        return Err(AuthorizationError::new(
            "REAUTHORIZATION_REQUIRED",
            decision.reason,
        ));
    }
    if input.original_revision != input.current_revision {
        return Err(AuthorizationError::new(
            "REAUTHORIZATION_REQUIRED",
            "application revision changed while offline",
        ));
    }
    Ok(decision)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionClaim {
    pub execution_id: String,
    pub workload_id: String,
    pub worker: Subject,
    pub grant: SignedGrant,
}
#[derive(Default)]
pub struct ClaimRegistry {
    claimed: BTreeSet<String>,
}
impl ClaimRegistry {
    pub fn claim(
        &mut self,
        claim: &ExecutionClaim,
        request: &AuthorizationRequest,
        signer: &GrantSigner,
        grants: &BTreeMap<String, SignedGrant>,
        revoked: &RevocationState,
    ) -> Result<AuthorizationDecision, AuthorizationError> {
        if !matches!(&claim.worker, Subject::Worker(_))
            || claim.worker != request.subject
            || request.context.execution_id.as_ref() != Some(&claim.execution_id)
            || request.context.workload_id.as_ref() != Some(&claim.workload_id)
        {
            return Err(AuthorizationError::new(
                "GRANT_SCOPE_DENIED",
                "worker is not assigned to this workload execution",
            ));
        }
        if self.claimed.contains(&claim.execution_id) {
            return Err(AuthorizationError::new(
                "GRANT_INVALID",
                "execution already claimed",
            ));
        }
        let decision = authorize(request, Some(&claim.grant), signer, grants, revoked);
        if !decision.allowed {
            return Err(AuthorizationError::new(
                "GRANT_SCOPE_DENIED",
                decision.reason,
            ));
        }
        self.claimed.insert(claim.execution_id.clone());
        Ok(decision)
    }
}

pub fn validate_resource_uri(uri: &str, application_id: &str) -> Result<(), AuthorizationError> {
    let prefix = format!("flow://{application_id}/");
    let root = format!("flow://{application_id}");
    if (uri != root && !uri.starts_with(&prefix))
        || uri.contains("..")
        || uri.contains('?')
        || uri.contains('#')
    {
        return Err(AuthorizationError::new(
            "GRANT_SCOPE_DENIED",
            "resource is not a stable application-scoped FeltDB identity",
        ));
    }
    Ok(())
}
fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn decode_hex(value: &str) -> Result<Vec<u8>, AuthorizationError> {
    if value.len() % 2 != 0 {
        return Err(AuthorizationError::new(
            "GRANT_INVALID",
            "invalid hex encoding",
        ));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| AuthorizationError::new("GRANT_INVALID", "invalid hex encoding"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn signer() -> GrantSigner {
        GrantSigner::new("test", vec![7; 32]).unwrap()
    }
    fn grant() -> Grant {
        Grant {
            contract_version: 1,
            id: "g1".into(),
            issuer: Subject::Human("owner".into()),
            subject: Subject::Agent("coordinator".into()),
            tenant_id: "relief".into(),
            application_id: "relief-fabric".into(),
            capabilities: ["state:read".into(), "state:write".into()].into(),
            resources: vec![ResourceScope {
                kind: ResourceScopeKind::ResourcePrefix,
                uri: "flow://relief-fabric/incidents/*".into(),
            }],
            constraints: Default::default(),
            issued_at: 100,
            expires_at: 300,
            parent_grant: None,
            revision: 1,
            status: GrantStatus::Active,
        }
    }
    fn request() -> AuthorizationRequest {
        AuthorizationRequest {
            subject: Subject::Agent("coordinator".into()),
            tenant_id: "relief".into(),
            application_id: "relief-fabric".into(),
            resource: Resource {
                uri: "flow://relief-fabric/incidents/123".into(),
                attributes: BTreeMap::new(),
            },
            capability: "state:read".into(),
            context: AuthorizationContext {
                timestamp: 200,
                ..Default::default()
            },
            grant_id: Some("g1".into()),
            subject_attributes: BTreeMap::new(),
        }
    }
    #[test]
    fn signed_scope_and_tampering() {
        let signer = signer();
        let signed = signer.sign(grant()).unwrap();
        let grants = [("g1".into(), signed.clone())].into();
        assert!(
            authorize(
                &request(),
                Some(&signed),
                &signer,
                &grants,
                &Default::default()
            )
            .allowed
        );
        let mut bad = signed.clone();
        bad.grant.capabilities.insert("secret:read".into());
        assert_eq!(
            verify_grant(&bad, &signer, &request(), &grants, &Default::default())
                .unwrap_err()
                .code,
            "GRANT_INVALID"
        );
    }
    #[test]
    fn cross_boundary_and_context_denied() {
        let signer = signer();
        let signed = signer.sign(grant()).unwrap();
        let grants = [("g1".into(), signed.clone())].into();
        let mut r = request();
        r.application_id = "other".into();
        assert!(!authorize(&r, Some(&signed), &signer, &grants, &Default::default()).allowed);
        let mut r = request();
        r.resource.uri = "flow://relief-fabric/assignments/1".into();
        assert!(!authorize(&r, Some(&signed), &signer, &grants, &Default::default()).allowed);
    }
    #[test]
    fn delegation_only_narrows() {
        let signer = signer();
        let parent = signer.sign(grant()).unwrap();
        let mut child = grant();
        child.id = "g2".into();
        child.issuer = child.subject.clone();
        child.subject = Subject::Workload("w1".into());
        child.capabilities = ["state:write".into()].into();
        child.resources = vec![ResourceScope {
            kind: ResourceScopeKind::Resource,
            uri: "flow://relief-fabric/incidents/123".into(),
        }];
        child.expires_at = 250;
        child.parent_grant = Some("g1".into());
        assert!(child.is_subset_of(&parent.grant));
        child.capabilities.insert("secret:read".into());
        assert!(!child.is_subset_of(&parent.grant));
    }
    #[test]
    fn expiration_and_parent_revocation_propagate() {
        let signer = signer();
        let signed = signer.sign(grant()).unwrap();
        let grants = [("g1".into(), signed.clone())].into();
        let mut r = request();
        r.context.timestamp = 301;
        assert!(
            authorize(&r, Some(&signed), &signer, &grants, &Default::default())
                .reason
                .starts_with("GRANT_EXPIRED")
        );
        let mut revoked = RevocationState::default();
        revoked.grants.insert("g1".into());
        assert!(
            authorize(&request(), Some(&signed), &signer, &grants, &revoked)
                .reason
                .starts_with("GRANT_REVOKED")
        );
    }
    #[test]
    fn connection_use_never_implies_secret_read() {
        let signer = signer();
        let mut g = grant();
        g.capabilities = ["connection:use".into()].into();
        g.resources = vec![ResourceScope {
            kind: ResourceScopeKind::Connection,
            uri: "flow://relief-fabric/connections/weather".into(),
        }];
        let signed = signer.sign(g).unwrap();
        let grants = [("g1".into(), signed.clone())].into();
        let mut r = request();
        r.resource.uri = "flow://relief-fabric/connections/weather".into();
        r.capability = "connection:use".into();
        assert!(authorize(&r, Some(&signed), &signer, &grants, &Default::default()).allowed);
        r.capability = "secret:read".into();
        assert!(!authorize(&r, Some(&signed), &signer, &grants, &Default::default()).allowed);
    }
    #[test]
    fn offline_reconciliation_rechecks_revision_and_revocation() {
        let signer = signer();
        let signed = signer.sign(grant()).unwrap();
        let grants = [("g1".into(), signed.clone())].into();
        let input = ReconciliationRequest {
            original_grant: signed,
            current_request: request(),
            original_revision: "r1".into(),
            current_revision: "r2".into(),
        };
        assert_eq!(
            reauthorize_reconciliation(&input, &signer, &grants, &Default::default())
                .unwrap_err()
                .code,
            "REAUTHORIZATION_REQUIRED"
        );
    }
    #[test]
    fn public_verifier_and_worker_claim_enforce_assignment_and_replay() {
        let signer = signer();
        let verifier = GrantSigner::verifier_hex("test", &signer.public_key_hex()).unwrap();
        let mut g = grant();
        g.subject = Subject::Worker("worker-42".into());
        g.capabilities = ["workload:claim".into()].into();
        g.resources = vec![ResourceScope {
            kind: ResourceScopeKind::Workload,
            uri: "flow://relief-fabric/workloads/w1".into(),
        }];
        let signed = signer.sign(g).unwrap();
        assert!(verifier.verify_signature(&signed).is_ok());
        let grants = [("g1".into(), signed.clone())].into();
        let mut request = request();
        request.subject = Subject::Worker("worker-42".into());
        request.capability = "workload:claim".into();
        request.resource.uri = "flow://relief-fabric/workloads/w1".into();
        request.context.execution_id = Some("e1".into());
        request.context.workload_id = Some("w1".into());
        let claim = ExecutionClaim {
            execution_id: "e1".into(),
            workload_id: "w1".into(),
            worker: request.subject.clone(),
            grant: signed,
        };
        let mut registry = ClaimRegistry::default();
        assert!(registry
            .claim(&claim, &request, &verifier, &grants, &Default::default())
            .is_ok());
        assert_eq!(
            registry
                .claim(&claim, &request, &verifier, &grants, &Default::default())
                .unwrap_err()
                .message,
            "execution already claimed"
        );
    }
}
