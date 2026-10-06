//! Kernel-side helpers for the shared bus [`TestHarness`].
//!
//! The harness itself lives in `jinn-testutil` because it is a pure test
//! fixture over the message fabric. Building an [`ActorDeps`] from it needs
//! the kernel's [`Services`] container, so that part lives here as a trait:
//! `jinn-testutil` stays kernel-free and cannot depend on `jinn-kernel`
//! without a dependency cycle.
//!
//! Import this trait wherever a test needs `harness.services()` or
//! `harness.actor_deps()`.

use jinn_testutil::bus_harness::TestHarness;

use crate::common::actor_deps::ActorDeps;
use crate::common::services::Services;

/// Kernel-side builders for a bus [`TestHarness`].
pub trait HarnessServices {
    /// Build a [`Services`] with the harness bus wired into a test instance.
    ///
    /// This creates a `Services::new_fake()` and replaces its bus and trouper
    /// system with the harness ones, so actors use the same bus AND the same
    /// fabric the test is publishing on.
    fn services(&self) -> impl std::future::Future<Output = Services> + Send;

    /// Build an [`ActorDeps`] with the harness bus wired into a test [`Services`].
    ///
    /// This creates a `Services::new_fake()` and replaces its bus and trouper
    /// system with the harness ones, so actors use the same bus AND the same
    /// fabric the test is publishing on.
    fn actor_deps(&self) -> impl std::future::Future<Output = ActorDeps> + Send;
}

impl HarnessServices for TestHarness {
    async fn services(&self) -> Services {
        let mut services = Services::new_fake().await;
        services.bus = self.bus();
        services.trouper_system = self.system().clone();
        // The bridge captured `new_fake`'s own bus at construction; left
        // alone it publishes route-action closures into a system no actor
        // ever joined. Rebuild it over the harness bus so a published
        // closure and a direct publish land on the same fabric.
        services.bridge = crate::common::bridge::Bridge::new(services.bus.clone());
        services
    }

    async fn actor_deps(&self) -> ActorDeps {
        ActorDeps {
            services: self.services().await,
        }
    }
}
