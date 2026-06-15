//! The R6 scheduler binding (execution-context phase3/10 scheduling): bind each
//! macro-emitted [`ScheduledMethodDescriptor`] to a live [`Trigger`] + a
//! fire-the-body factory, [`SchedulerCore::register`] them at `after_init`, and
//! [`SchedulerCore::arm`] the wheel at the SmartInitializing barrier (refresh R6)
//! so a fire never hits a half-built graph.
//!
//! This RESOLVES the cross-crate scheduling NOTE the macros left
//! (`leaf-codegen/src/scheduling.rs`: the public `__leaf_scheduled_<Bean>_<Method>`
//! [`ScheduledMethodDescriptor`] const + the `SCHEDULED` identity row are JOINed
//! here into a `(Trigger, body)` registration on the live [`SchedulerCore`]).
//!
//! ## The pairing JOIN
//!
//! The macro can emit the const descriptor (bean + method + [`TriggerSpec`]) but
//! NOT the body thunk (it needs the live bean `Ref` + the typed method call). The
//! binary (`#[leaf::main]`) supplies one [`ScheduledPairing`] per `#[scheduled]`
//! method, pairing the descriptor with a body-factory the binding fires per tick.
//!
//! ## Trigger resolution
//!
//! [`TriggerSpec::FixedRate`]/[`TriggerSpec::FixedDelay`] resolve to leaf-core's
//! built-in [`FixedRateTrigger`]/[`FixedDelayTrigger`]. [`TriggerSpec::Cron`] is
//! NOT parsed here (leaf-boot does not depend on leaf-cron — the 6/7-field
//! calendar engine is a downstream crate's concern); a `Cron` spec resolves via
//! an optional [`CronTriggerFactory`] hook the binary installs (the crate that
//! force-links leaf-cron supplies it), else it is a loud [`LeafError`].

use std::sync::Arc;

use leaf_core::{
    BoxFuture, FixedDelayTrigger, FixedRateTrigger, LeafError, ScheduledMethodDescriptor,
    SchedulerCore, Trigger, TriggerSpec,
};

/// A fire-the-body factory: called per scheduled fire to mint a fresh `()`-typed
/// future (the macro-emitted body that resolves the live bean `Ref` + invokes the
/// typed method).
pub type ScheduledBody = Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// A cron-trigger factory hook (installed by the crate that force-links
/// leaf-cron): parse a cron expression into a live [`Trigger`]. leaf-boot does not
/// depend on leaf-cron, so a `#[scheduled(cron = "…")]` task requires this hook.
pub type CronTriggerFactory =
    Arc<dyn Fn(&str) -> Result<Box<dyn Trigger>, LeafError> + Send + Sync>;

// ─────────────────────────────── ScheduledPairing ───────────────────────────

/// The macro→runtime scheduled-task JOIN row (the scheduling analogue of
/// [`SeedPairing`](crate::SeedPairing)): pairs the macro-emitted const
/// [`ScheduledMethodDescriptor`] (bean + method + [`TriggerSpec`]) with the
/// body-factory the macro cannot emit as a const (it needs the live bean `Ref`).
pub struct ScheduledPairing {
    /// The macro-emitted const descriptor (the `__leaf_scheduled_*` pairing const).
    pub descriptor: ScheduledMethodDescriptor,
    /// The fire-the-body factory (resolves the live bean + invokes the method).
    pub body: ScheduledBody,
}

impl ScheduledPairing {
    /// Build a pairing from the macro descriptor + the body-factory.
    #[must_use]
    pub fn new(descriptor: ScheduledMethodDescriptor, body: ScheduledBody) -> Self {
        ScheduledPairing { descriptor, body }
    }
}

impl std::fmt::Debug for ScheduledPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduledPairing")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// Register every scheduled task onto the live [`SchedulerCore`] (refresh R6
/// `after_init`, BEFORE the arm): resolve each [`TriggerSpec`] into a live
/// [`Trigger`] and `register` `(descriptor, trigger, body)`.
///
/// The wheel is NOT armed here — arming happens at the SmartInitializing barrier
/// via [`SchedulerCore::arm`], after every singleton is published.
///
/// # Errors
/// A [`LeafError`] if a `Cron` spec has no installed [`CronTriggerFactory`], a
/// cron expression fails to parse, or `register` rejects the task.
pub fn register_scheduled(
    scheduler: &dyn SchedulerCore,
    tasks: Vec<ScheduledPairing>,
    cron_factory: Option<&CronTriggerFactory>,
) -> Result<usize, LeafError> {
    let mut registered = 0;
    for ScheduledPairing { descriptor, body } in tasks {
        let trigger = resolve_trigger(&descriptor, cron_factory)?;
        scheduler.register(descriptor, trigger, body)?;
        registered += 1;
    }
    Ok(registered)
}

/// Resolve a [`TriggerSpec`] into a live boxed [`Trigger`].
///
/// # Errors
/// A [`LeafError`] for a `Cron` spec with no installed factory (or a parse fault).
pub fn resolve_trigger(
    descriptor: &ScheduledMethodDescriptor,
    cron_factory: Option<&CronTriggerFactory>,
) -> Result<Box<dyn Trigger>, LeafError> {
    match descriptor.spec {
        TriggerSpec::FixedRate { period, initial_delay } => {
            Ok(Box::new(FixedRateTrigger::new(period).with_initial_delay(initial_delay)))
        }
        TriggerSpec::FixedDelay { delay, initial_delay } => {
            Ok(Box::new(FixedDelayTrigger::new(delay).with_initial_delay(initial_delay)))
        }
        TriggerSpec::Cron(expr) => match cron_factory {
            Some(factory) => factory(expr),
            None => Err(missing_cron_factory(expr)),
        },
    }
}

fn missing_cron_factory(expr: &str) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConfigIo).caused_by(leaf_core::Cause::plain(
        "refresh R6: resolving a #[scheduled(cron = ...)] trigger",
        format!(
            "no cron-trigger factory is installed for the cron expression `{expr}` \
             (leaf-boot does not parse cron — force-link leaf-cron and install a \
             CronTriggerFactory via RunUnit::with_cron_factory)"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use leaf_core::{ContractId, MethodKey};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // A recording fake scheduler: counts registers + tracks arm/disarm state.
    #[derive(Default)]
    struct FakeScheduler {
        registered: AtomicU32,
        armed: Mutex<bool>,
    }
    impl SchedulerCore for FakeScheduler {
        fn register(
            &self,
            _descriptor: ScheduledMethodDescriptor,
            _trigger: Box<dyn Trigger>,
            _body: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
        ) -> Result<(), LeafError> {
            self.registered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn arm(&self) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async move {
                *self.armed.lock().unwrap() = true;
                Ok(())
            })
        }
        fn disarm(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                *self.armed.lock().unwrap() = false;
            })
        }
    }

    fn body() -> ScheduledBody {
        Box::new(|| Box::pin(async {}))
    }

    fn descriptor(spec: TriggerSpec) -> ScheduledMethodDescriptor {
        ScheduledMethodDescriptor::new(
            ContractId::of("test::Worker"),
            MethodKey::of("test::Worker::tick"),
            spec,
        )
    }

    #[test]
    fn register_scheduled_binds_fixed_rate_and_fixed_delay_tasks() {
        let scheduler = FakeScheduler::default();
        let tasks = vec![
            ScheduledPairing::new(
                descriptor(TriggerSpec::FixedRate {
                    period: Duration::from_secs(1),
                    initial_delay: Duration::ZERO,
                }),
                body(),
            ),
            ScheduledPairing::new(
                descriptor(TriggerSpec::FixedDelay {
                    delay: Duration::from_secs(2),
                    initial_delay: Duration::ZERO,
                }),
                body(),
            ),
        ];
        let n = register_scheduled(&scheduler, tasks, None).unwrap();
        assert_eq!(n, 2, "both fixed tasks registered");
        assert_eq!(scheduler.registered.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_cron_task_without_a_factory_is_a_loud_error() {
        let scheduler = FakeScheduler::default();
        let tasks = vec![ScheduledPairing::new(descriptor(TriggerSpec::Cron("* * * * * *")), body())];
        let err = register_scheduled(&scheduler, tasks, None)
            .expect_err("cron needs a factory leaf-boot does not provide");
        assert_eq!(err.kind, leaf_core::ErrorKind::ConfigIo);
        assert_eq!(scheduler.registered.load(Ordering::SeqCst), 0, "nothing registered on failure");
    }

    #[test]
    fn a_cron_task_with_an_installed_factory_registers() {
        let scheduler = FakeScheduler::default();
        let factory: CronTriggerFactory = Arc::new(|_expr: &str| {
            Ok(Box::new(FixedRateTrigger::new(Duration::from_secs(60))) as Box<dyn Trigger>)
        });
        let tasks = vec![ScheduledPairing::new(descriptor(TriggerSpec::Cron("0 0 * * * *")), body())];
        let n = register_scheduled(&scheduler, tasks, Some(&factory)).unwrap();
        assert_eq!(n, 1, "the cron task registered via the factory");
    }

    #[test]
    fn arm_then_disarm_toggles_the_wheel() {
        let scheduler = FakeScheduler::default();
        assert!(!*scheduler.armed.lock().unwrap(), "starts disarmed");
        block(scheduler.arm()).unwrap();
        assert!(*scheduler.armed.lock().unwrap(), "armed after the barrier");
        block(scheduler.disarm());
        assert!(!*scheduler.armed.lock().unwrap(), "disarmed at teardown step 1");
    }

    #[test]
    fn resolve_trigger_maps_each_spec_kind() {
        // FixedRate / FixedDelay resolve natively; Cron requires the factory.
        assert!(resolve_trigger(
            &descriptor(TriggerSpec::FixedRate {
                period: Duration::from_millis(10),
                initial_delay: Duration::ZERO
            }),
            None
        )
        .is_ok());
        assert!(resolve_trigger(
            &descriptor(TriggerSpec::FixedDelay {
                delay: Duration::from_millis(10),
                initial_delay: Duration::ZERO
            }),
            None
        )
        .is_ok());
        assert!(resolve_trigger(&descriptor(TriggerSpec::Cron("* * * * * *")), None).is_err());
    }
}
