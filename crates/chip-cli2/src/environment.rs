//! Adds the cli2 backend to an environment, through Chip's own environment contract.
//!
//! [`Cli2EnvironmentProvider`] wraps another [`EnvironmentProvider`] (the local project directory,
//! in practice). Every environment it hands out has the inner one's capabilities and invariants
//! plus `cli2.tasks` and `cli2.run`, and nothing else changes: the same `CapabilitySet` routing
//! by declared id, the same observation invariants, the same release rules.

use std::sync::Arc;

use chip_core::{
    CapabilitySet, EnvironmentDescription, EnvironmentError, EnvironmentId, EnvironmentProvider,
    ObservationInvariant, WorkEnvironment, WorkId,
};

use crate::{Cli2Backend, Cli2Config};

pub struct Cli2EnvironmentProvider {
    inner: Arc<dyn EnvironmentProvider>,
    cfg: Cli2Config,
}

impl Cli2EnvironmentProvider {
    pub fn new(inner: Arc<dyn EnvironmentProvider>, cfg: Cli2Config) -> Self {
        Self { inner, cfg }
    }
}

struct Cli2Environment {
    inner: Arc<dyn WorkEnvironment>,
    set: Arc<CapabilitySet>,
}

impl WorkEnvironment for Cli2Environment {
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
}

#[async_trait::async_trait]
impl EnvironmentProvider for Cli2EnvironmentProvider {
    fn isolation_capacity(&self) -> usize {
        self.inner.isolation_capacity()
    }

    async fn acquire(&self, work: &WorkId) -> Result<Arc<dyn WorkEnvironment>, EnvironmentError> {
        let inner = self.inner.acquire(work).await?;
        // A fresh set per environment; the inner capabilities are cloned, never altered.
        let set = (*inner.capabilities())
            .clone()
            .with(Arc::new(Cli2Backend::new(self.cfg.clone())));
        Ok(Arc::new(Cli2Environment {
            inner,
            set: Arc::new(set),
        }))
    }

    fn release(&self, work: &WorkId, environment: &EnvironmentId) {
        self.inner.release(work, environment);
    }
}
