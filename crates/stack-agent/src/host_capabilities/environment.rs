//! Adds the host's capabilities to an environment, through Chip's own environment contract.
//!
//! Like `chip_cli2::Cli2EnvironmentProvider`, [`HostEnvironmentProvider`] wraps another
//! [`EnvironmentProvider`] and changes nothing but the capability set: the same routing by declared
//! id, the same observation invariants, the same release rules. With `keep_builtin` the host's
//! capabilities are added to the inner environment's; without it they are the only ones, and the
//! built-in project capabilities do not exist for this work (they are not hidden: absent).

use std::sync::Arc;

use chip_core::{
    CapabilitySet, EnvironmentDescription, EnvironmentError, EnvironmentId, EnvironmentProvider,
    ObservationInvariant, OperatorCapabilities, WorkEnvironment, WorkId,
};

use super::{HostBackend, HostCapabilities};

pub struct HostEnvironmentProvider {
    inner: Arc<dyn EnvironmentProvider>,
    host: Arc<HostCapabilities>,
}

impl HostEnvironmentProvider {
    pub fn new(inner: Arc<dyn EnvironmentProvider>, host: Arc<HostCapabilities>) -> Self {
        Self { inner, host }
    }
}

struct HostEnvironment {
    inner: Arc<dyn WorkEnvironment>,
    set: Arc<CapabilitySet>,
    operator: OperatorCapabilities,
}

impl WorkEnvironment for HostEnvironment {
    fn id(&self) -> &EnvironmentId {
        self.inner.id()
    }

    fn capabilities(&self) -> Arc<CapabilitySet> {
        self.set.clone()
    }

    fn observation_invariants(&self) -> Vec<Arc<dyn ObservationInvariant>> {
        self.inner.observation_invariants()
    }

    fn description(&self) -> EnvironmentDescription {
        self.inner.description()
    }

    fn operator(&self) -> OperatorCapabilities {
        self.operator.clone()
    }
}

#[async_trait::async_trait]
impl EnvironmentProvider for HostEnvironmentProvider {
    fn isolation_capacity(&self) -> usize {
        self.inner.isolation_capacity()
    }

    async fn acquire(&self, work: &WorkId) -> Result<Arc<dyn WorkEnvironment>, EnvironmentError> {
        let inner = self.inner.acquire(work).await?;
        let backend = Arc::new(HostBackend::new(self.host.clone()));
        // A fresh set per environment; the inner capabilities are cloned, never altered.
        let set = if self.host.keep_builtin {
            (*inner.capabilities()).clone().with(backend)
        } else {
            CapabilitySet::new().with(backend)
        };
        let operator = OperatorCapabilities {
            offered: self
                .host
                .entries
                .iter()
                .map(|e| (e.descriptor.id.clone(), e.descriptor.effect))
                .collect(),
            replaces_builtin: !self.host.keep_builtin,
            required_success: self.host.required_success.clone(),
        };
        Ok(Arc::new(HostEnvironment {
            inner,
            set: Arc::new(set),
            operator,
        }))
    }

    fn release(&self, work: &WorkId, environment: &EnvironmentId) {
        self.inner.release(work, environment);
    }
}
