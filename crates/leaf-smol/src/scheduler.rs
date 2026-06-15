//! [`SmolSchedulerCore`] — the ONE reactive timer-wheel backing
//! [`SchedulerCore`] over smol.
//!
//! The mirror of `leaf-tokio`'s `TokioSchedulerCore` (phase3/10 `scheduling`,
//! ADR-07 5c/5f). It is NOT a second executor: it owns WHEN (a min-heap of
//! `(next_fire, task_id)` + a single driver that sleeps to the global earliest on
//! a reactive [`smol::Timer`]) and delegates WHERE to the injected [`Spawner`] —
//! the due body is SPAWNED, never run on the driver (the structural fix for
//! Spring's single-thread serialization gotcha).
//!
//! Reactivity: there is NO interval spin. The driver awaits a [`smol::Timer`] to
//! the earliest fire, raced against a wake notify channel; a [`register`] that
//! introduces an earlier fire, or [`disarm`], wakes the driver early. On hot
//! paths nothing polls; the only "loop" is the sanctioned cold-path reactive
//! timer the design names.
//!
//! Overlap + completion feedback mirror the tokio core exactly:
//! [`OverlapPolicy::SkipIfRunning`] (the default) skips a fire that is due while
//! the prior body is still in flight; the fixed-delay completion-feedback
//! contract (next fire measured from completion, not the scheduled time) is
//! honored by a bookkeeping task that awaits the body's [`SpawnHandle`] and
//! re-arms the trigger from the ACTUAL completion `Instant`.
//!
//! Unlike tokio there is no virtual-clock test mode, so the wheel runs on the
//! real [`std::time::Instant`] timeline and tests use short real durations.
//!
//! [`register`]: SchedulerCore::register
//! [`disarm`]: SchedulerCore::disarm

use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use leaf_core::{
    BoxFuture, DropPolicy, LeafError, OverlapPolicy, ScheduledMethodDescriptor, SchedulerCore,
    Spawner, Trigger, TriggerContext, TriggerSpec,
};

use crate::notify::Notify;

/// A registered task: its descriptor, trigger, body factory, and live feedback.
struct Task {
    #[allow(dead_code)]
    descriptor: ScheduledMethodDescriptor,
    overlap: OverlapPolicy,
    trigger: Box<dyn Trigger>,
    body: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    /// The feedback the next `next_fire` reads (updated as fires run/complete).
    ctx: Mutex<TriggerContext>,
    /// `true` while a body for this task is in flight (overlap bookkeeping).
    running: AtomicBool,
    /// Generation: bumped on every re-arm so a stale heap entry is discarded.
    generation: Mutex<u64>,
    /// Whether the next fire is measured from the body's COMPLETION (fixed-delay)
    /// rather than the scheduled time (fixed-rate / cron) — the ONE place the
    /// completion-feedback contract is encoded.
    rearm_on_completion: bool,
}

/// A heap entry; the soonest fire has the GREATEST priority (we reverse `at` so
/// the max-heap `BinaryHeap` yields the earliest first).
#[derive(Clone, Copy)]
struct Pending {
    at: Instant,
    task: usize,
    stamp: u64,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at
    }
}
impl Eq for Pending {}
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.at.cmp(&self.at)
    }
}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Mutable wheel state guarded by one `Mutex`.
struct Inner {
    tasks: Vec<Arc<Task>>,
    heap: BinaryHeap<Pending>,
    disarmed: bool,
}

/// The smol-backed [`SchedulerCore`]: a single reactive timer-wheel that spawns
/// due bodies onto the injected [`Spawner`].
#[derive(Clone)]
pub struct SmolSchedulerCore {
    inner: Arc<Mutex<Inner>>,
    notify: Notify,
    spawner: Arc<dyn Spawner>,
    armed: Arc<AtomicBool>,
}

impl SmolSchedulerCore {
    /// Construct a scheduler that spawns bodies onto `spawner`.
    #[must_use]
    pub fn new(spawner: Arc<dyn Spawner>) -> Self {
        SmolSchedulerCore {
            inner: Arc::new(Mutex::new(Inner {
                tasks: Vec::new(),
                heap: BinaryHeap::new(),
                disarmed: false,
            })),
            notify: Notify::new(),
            spawner,
            armed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Schedule `task`'s next fire from `now`, pushing it onto the heap.
    /// Returns `true` if a fire was scheduled (a `None` trigger = no further
    /// fire, so the task quiesces).
    fn schedule_next(inner: &mut Inner, task_idx: usize, now: Instant) -> bool {
        let task = &inner.tasks[task_idx];
        let ctx = *task.ctx.lock().expect("ctx mutex");
        let Some(at) = task.trigger.next_fire(now, ctx) else {
            return false;
        };
        let stamp = {
            let mut g = task.generation.lock().expect("gen mutex");
            *g += 1;
            *g
        };
        inner.heap.push(Pending {
            at,
            task: task_idx,
            stamp,
        });
        true
    }

    /// The driver loop: sleep to the earliest fire, fire it, re-arm. Reactive —
    /// it parks on a [`smol::Timer`] raced against the wake notify channel, never
    /// spins.
    async fn drive(self) {
        loop {
            let next_at = {
                let inner = self.inner.lock().expect("wheel mutex");
                if inner.disarmed {
                    return;
                }
                inner.heap.peek().map(|p| p.at)
            };

            match next_at {
                None => {
                    // Nothing scheduled: park until a register/disarm notifies.
                    self.notify.notified().await;
                }
                Some(at) => {
                    let delay = at.saturating_duration_since(Instant::now());
                    if !delay.is_zero() {
                        // Reactive sleep to the earliest fire; a notify (earlier
                        // fire registered / disarm) cancels the sleep early.
                        let timer = smol::Timer::after(delay);
                        futures::pin_mut!(timer);
                        let woke = self.notify.notified();
                        futures::pin_mut!(woke);
                        match futures::future::select(woke, timer).await {
                            // Notified early (new earlier fire / disarm): re-loop.
                            futures::future::Either::Left(_) => continue,
                            // Timer elapsed: fire what is due.
                            futures::future::Either::Right(_) => {}
                        }
                    }
                    self.fire_due(at);
                }
            }
        }
    }

    /// Pop and fire every entry due at or before `threshold` (the wake target),
    /// honoring overlap + re-arming. `threshold` is the trigger-timeline instant
    /// the driver slept until; it is used as the synthetic "now" so the wheel
    /// stays on the `Trigger`'s `std::time::Instant` cadence.
    fn fire_due(&self, threshold: Instant) {
        loop {
            let pending = {
                let mut inner = self.inner.lock().expect("wheel mutex");
                if inner.disarmed {
                    return;
                }
                match inner.heap.peek().copied() {
                    Some(p) if p.at <= threshold => {
                        inner.heap.pop();
                        Some(p)
                    }
                    _ => None,
                }
            };
            let Some(p) = pending else { return };

            // Discard a stale heap entry (a re-arm bumped the generation).
            {
                let inner = self.inner.lock().expect("wheel mutex");
                let task = &inner.tasks[p.task];
                let cur = *task.generation.lock().expect("gen mutex");
                if cur != p.stamp {
                    continue;
                }
            }

            self.fire_one(p.task, p.at, p.at);
        }
    }

    /// Fire one task: record the scheduled/actual-fire feedback, spawn the body
    /// (or skip on overlap), and re-arm per the trigger's feedback discipline.
    fn fire_one(&self, task_idx: usize, scheduled: Instant, actual_fire: Instant) {
        let task = {
            let inner = self.inner.lock().expect("wheel mutex");
            Arc::clone(&inner.tasks[task_idx])
        };

        let still_running = task.running.load(Ordering::SeqCst);
        if still_running && task.overlap == OverlapPolicy::SkipIfRunning {
            // Skip this fire. Record the skipped scheduled time so a rate cadence
            // keeps stepping; re-arm a rate task now (a delay task re-arms when
            // the in-flight body completes, so leave it to the completion path).
            {
                let mut ctx = task.ctx.lock().expect("ctx mutex");
                ctx.last_scheduled = Some(scheduled);
            }
            if !task.rearm_on_completion {
                self.rearm(task_idx, actual_fire);
            }
            return;
        }

        // Record feedback BEFORE spawning so the next fire's trigger sees it.
        {
            let mut ctx = task.ctx.lock().expect("ctx mutex");
            ctx.last_scheduled = Some(scheduled);
            ctx.last_actual_fire = Some(actual_fire);
        }
        task.running.store(true, Ordering::SeqCst);

        // Spawn the body onto the injected Spawner (NEVER run on the driver).
        let handle = self.spawner.spawn((task.body)());

        // Bookkeeping: await completion to clear `running`, record completion
        // feedback, and (for fixed-delay) re-arm from the ACTUAL completion.
        let this = self.clone();
        let task_for_bk = Arc::clone(&task);
        let bk = async move {
            let _ = handle.await;
            let completion = Instant::now();
            {
                let mut ctx = task_for_bk.ctx.lock().expect("ctx mutex");
                ctx.last_completion = Some(completion);
            }
            task_for_bk.running.store(false, Ordering::SeqCst);
            if task_for_bk.rearm_on_completion {
                this.rearm(task_idx, completion);
            }
        };
        // Fire-and-forget bookkeeping: it must outlive the body handle so the
        // completion feedback is recorded (and the delay cadence re-arms).
        self.spawner
            .spawn(Box::pin(bk))
            .with_policy(DropPolicy::Detach)
            .detach();

        // Rate-like triggers (fixed-rate / cron) re-arm eagerly from the
        // scheduled time so a slow body does NOT push the cadence.
        if !task.rearm_on_completion {
            self.rearm(task_idx, actual_fire);
        }
    }

    /// Compute + push the next fire for `task_idx` relative to `from`, bumping the
    /// generation so any superseded heap entry is discarded, and wake the driver.
    fn rearm(&self, task_idx: usize, from: Instant) {
        let mut inner = self.inner.lock().expect("wheel mutex");
        if inner.disarmed {
            return;
        }
        let task = Arc::clone(&inner.tasks[task_idx]);
        let ctx = *task.ctx.lock().expect("ctx mutex");
        if let Some(at) = task.trigger.next_fire(from, ctx) {
            let stamp = {
                let mut g = task.generation.lock().expect("gen mutex");
                *g += 1;
                *g
            };
            inner.heap.push(Pending {
                at,
                task: task_idx,
                stamp,
            });
            drop(inner);
            self.notify.notify();
        }
    }
}

impl SchedulerCore for SmolSchedulerCore {
    fn register(
        &self,
        descriptor: ScheduledMethodDescriptor,
        trigger: Box<dyn Trigger>,
        body: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    ) -> Result<(), LeafError> {
        // The completion-feedback discipline is data on the descriptor's spec:
        // FixedDelay re-arms from the body's completion; FixedRate/Cron re-arm
        // from the scheduled time. (Getting this wrong silently turns fixedDelay
        // into fixedRate — the documented correctness contract.)
        let rearm_on_completion = matches!(descriptor.spec, TriggerSpec::FixedDelay { .. });
        let mut inner = self.inner.lock().expect("wheel mutex");
        let task = Arc::new(Task {
            overlap: descriptor.overlap,
            descriptor,
            trigger,
            body,
            ctx: Mutex::new(TriggerContext::initial()),
            running: AtomicBool::new(false),
            generation: Mutex::new(0),
            rearm_on_completion,
        });
        let idx = inner.tasks.len();
        inner.tasks.push(task);
        // If already armed, schedule the first fire immediately and wake the
        // driver; otherwise `arm` schedules all initial fires.
        if self.armed.load(Ordering::SeqCst) {
            let now = Instant::now();
            SmolSchedulerCore::schedule_next(&mut inner, idx, now);
            drop(inner);
            self.notify.notify();
        }
        Ok(())
    }

    fn arm(&self) -> BoxFuture<'_, Result<(), LeafError>> {
        Box::pin(async move {
            if self.armed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            // Schedule the first fire for every registered task.
            {
                let mut inner = self.inner.lock().expect("wheel mutex");
                let now = Instant::now();
                let n = inner.tasks.len();
                for idx in 0..n {
                    SmolSchedulerCore::schedule_next(&mut inner, idx, now);
                }
            }
            // Launch the single reactive driver (detached: it lives until disarm).
            self.spawner
                .spawn(Box::pin(self.clone().drive()))
                .with_policy(DropPolicy::Detach)
                .detach();
            self.notify.notify();
            Ok(())
        })
    }

    fn disarm(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            {
                let mut inner = self.inner.lock().expect("wheel mutex");
                inner.disarmed = true;
                inner.heap.clear();
            }
            // Wake the driver so it observes `disarmed` and exits.
            self.notify.notify();
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::SmolExecutionFacility;
    use leaf_core::{FixedDelayTrigger, FixedRateTrigger, MethodKey, TriggerSpec};
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    fn facility() -> Arc<dyn Spawner> {
        Arc::new(SmolExecutionFacility::new())
    }

    fn descriptor(name: &'static str) -> ScheduledMethodDescriptor {
        ScheduledMethodDescriptor::new(
            leaf_core::ContractId::of(name),
            MethodKey::of(name),
            TriggerSpec::FixedRate {
                period: Duration::from_millis(10),
                initial_delay: Duration::ZERO,
            },
        )
    }

    fn descriptor_delay(name: &'static str) -> ScheduledMethodDescriptor {
        ScheduledMethodDescriptor::new(
            leaf_core::ContractId::of(name),
            MethodKey::of(name),
            TriggerSpec::FixedDelay {
                delay: Duration::from_millis(10),
                initial_delay: Duration::ZERO,
            },
        )
    }

    /// Poll the count until `pred` holds or the deadline elapses (real time).
    async fn wait_until(deadline: Duration, mut pred: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if pred() {
                return true;
            }
            smol::Timer::after(Duration::from_millis(2)).await;
        }
        pred()
    }

    #[test]
    fn fixed_rate_fires_reactively_on_the_timer() {
        smol::block_on(async {
            let sched = SmolSchedulerCore::new(facility());
            let count = Arc::new(AtomicU32::new(0));
            let c = count.clone();
            let trig = FixedRateTrigger::new(Duration::from_millis(15))
                .with_initial_delay(Duration::from_millis(15));
            sched
                .register(
                    descriptor("app::Tick"),
                    Box::new(trig),
                    Box::new(move || {
                        let c = c.clone();
                        Box::pin(async move {
                            c.fetch_add(1, Ordering::SeqCst);
                        })
                    }),
                )
                .unwrap();
            sched.arm().await.unwrap();

            // Nothing has fired before the initial delay elapses (reactive).
            assert_eq!(count.load(Ordering::SeqCst), 0);

            // Within ~300ms the wheel should have fired several times.
            let fired = wait_until(Duration::from_millis(800), || {
                count.load(Ordering::SeqCst) >= 3
            })
            .await;
            assert!(
                fired,
                "fixed-rate must fire reactively (got {})",
                count.load(Ordering::SeqCst)
            );
            sched.disarm().await;
        });
    }

    #[test]
    fn fires_at_least_once_with_zero_initial_delay() {
        smol::block_on(async {
            let sched = SmolSchedulerCore::new(facility());
            let (tx, rx) = smol::channel::unbounded::<()>();
            let trig = FixedRateTrigger::new(Duration::from_secs(3600)); // far-apart
            sched
                .register(
                    descriptor("app::Once"),
                    Box::new(trig),
                    Box::new(move || {
                        let tx = tx.clone();
                        Box::pin(async move {
                            let _ = tx.send(()).await;
                        })
                    }),
                )
                .unwrap();
            sched.arm().await.unwrap();
            // First fire is at now+0; it must run promptly.
            let got = wait_until(Duration::from_millis(800), || !rx.is_empty()).await;
            assert!(got, "the immediate first fire must run");
            sched.disarm().await;
        });
    }

    #[test]
    fn fixed_delay_measures_from_completion() {
        smol::block_on(async {
            // A slow body (60ms) with a 10ms delay: the second fire must land
            // ~70ms after the first started, NOT 10ms (silent fixed-rate).
            let sched = SmolSchedulerCore::new(facility());
            let fires: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
            let f = fires.clone();
            let trig = FixedDelayTrigger::new(Duration::from_millis(10));
            sched
                .register(
                    descriptor_delay("app::Slow"),
                    Box::new(trig),
                    Box::new(move || {
                        let f = f.clone();
                        Box::pin(async move {
                            f.lock().unwrap().push(Instant::now());
                            smol::Timer::after(Duration::from_millis(60)).await;
                        })
                    }),
                )
                .unwrap();
            sched.arm().await.unwrap();

            // Wait for at least two fire-starts.
            let two = wait_until(Duration::from_secs(2), || {
                fires.lock().unwrap().len() >= 2
            })
            .await;
            assert!(
                two,
                "must fire at least twice (got {})",
                fires.lock().unwrap().len()
            );
            let starts = fires.lock().unwrap().clone();
            let gap = starts[1].duration_since(starts[0]);
            // Body 60ms + delay 10ms = ~70ms between START of fire 1 and fire 2.
            assert!(
                gap >= Duration::from_millis(55),
                "fixed-delay gap must be ~70ms (body+delay), got {gap:?} — \
                 a ~10ms gap would mean it degraded to fixed-rate"
            );
            sched.disarm().await;
        });
    }

    #[test]
    fn disarm_stops_further_fires() {
        smol::block_on(async {
            let sched = SmolSchedulerCore::new(facility());
            let count = Arc::new(AtomicU32::new(0));
            let c = count.clone();
            let trig = FixedRateTrigger::new(Duration::from_millis(10));
            sched
                .register(
                    descriptor("app::Stoppable"),
                    Box::new(trig),
                    Box::new(move || {
                        let c = c.clone();
                        Box::pin(async move {
                            c.fetch_add(1, Ordering::SeqCst);
                        })
                    }),
                )
                .unwrap();
            sched.arm().await.unwrap();
            // Let it fire a few times.
            let _ = wait_until(Duration::from_millis(500), || {
                count.load(Ordering::SeqCst) >= 2
            })
            .await;
            sched.disarm().await;
            // Allow in-flight bookkeeping to settle.
            smol::Timer::after(Duration::from_millis(30)).await;
            let after_disarm = count.load(Ordering::SeqCst);
            // No further fires after disarm.
            smol::Timer::after(Duration::from_millis(120)).await;
            assert_eq!(
                count.load(Ordering::SeqCst),
                after_disarm,
                "disarm must stop further fires"
            );
        });
    }

    #[test]
    fn skip_if_running_does_not_overlap() {
        smol::block_on(async {
            // A body that takes 80ms with a 10ms rate + SkipIfRunning: while one
            // body runs, due fires are skipped, so concurrency never exceeds 1.
            let sched = SmolSchedulerCore::new(facility());
            let concurrent = Arc::new(AtomicU32::new(0));
            let max_seen = Arc::new(AtomicU32::new(0));
            let cc = concurrent.clone();
            let mm = max_seen.clone();
            let trig = FixedRateTrigger::new(Duration::from_millis(10));
            sched
                .register(
                    descriptor("app::NonOverlap"),
                    Box::new(trig),
                    Box::new(move || {
                        let cc = cc.clone();
                        let mm = mm.clone();
                        Box::pin(async move {
                            let n = cc.fetch_add(1, Ordering::SeqCst) + 1;
                            mm.fetch_max(n, Ordering::SeqCst);
                            smol::Timer::after(Duration::from_millis(80)).await;
                            cc.fetch_sub(1, Ordering::SeqCst);
                        })
                    }),
                )
                .unwrap();
            sched.arm().await.unwrap();
            smol::Timer::after(Duration::from_millis(500)).await;
            assert_eq!(
                max_seen.load(Ordering::SeqCst),
                1,
                "SkipIfRunning must never run two bodies concurrently"
            );
            sched.disarm().await;
        });
    }
}
