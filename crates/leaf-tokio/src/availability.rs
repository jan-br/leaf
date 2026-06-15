//! The process-wide availability watch-cell.
//!
//! Core owns the [`AvailabilityHandle`] shape (two orthogonal watch cells —
//! liveness + readiness — over the ONE reactive watch primitive, events
//! phase3/12 5f). leaf-tokio, as the runtime integration, provides its ambient
//! home so a k8s liveness/readiness probe handler and the teardown
//! `Readiness→RefusingTraffic` flip (container-lifecycle phase3/13 teardown
//! step 1) reach the SAME cell without threading the handle through every API —
//! exactly as [`watch_run_state`](leaf_core::watch_run_state) is the ambient home
//! of the one `watch<RunState>` cell.
//!
//! Reads are lock-free point reads; subscription is reactive (woken on change),
//! NEVER an `is_running`/availability poll loop (charter §2.4).

use leaf_core::AvailabilityHandle;

static AVAILABILITY: once_cell::sync::Lazy<AvailabilityHandle> =
    once_cell::sync::Lazy::new(AvailabilityHandle::new);

/// The process-wide [`AvailabilityHandle`] (the ambient liveness/readiness cell).
///
/// Returns a clone of the ambient handle (cheap — it shares the two `WatchSender`
/// `Arc`s). The teardown template flips readiness through this; a probe handler
/// reads / subscribes through it.
#[must_use]
pub fn availability() -> AvailabilityHandle {
    AVAILABILITY.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{LivenessState, ReadinessState};

    #[tokio::test]
    async fn ambient_availability_round_trips_a_readiness_flip() {
        let a = availability();
        // Default once started: accepting traffic.
        assert_eq!(a.readiness(), ReadinessState::AcceptingTraffic);

        // Subscribe BEFORE the flip so the reactive `changed()` observes it.
        let mut rx = a.watch_readiness();
        let before = rx.borrow_and_update();
        let next = if before == ReadinessState::AcceptingTraffic {
            ReadinessState::RefusingTraffic
        } else {
            ReadinessState::AcceptingTraffic
        };
        a.set_readiness(next, "leaf-tokio-test");
        let got = rx.changed().await;
        assert_eq!(got, next);
        // A second handle from the ambient home sees the same state (one cell).
        assert_eq!(availability().readiness(), next);
    }

    #[tokio::test]
    async fn liveness_flip_is_visible_through_the_same_cell() {
        let a = availability();
        let mut rx = a.watch_liveness();
        let before = rx.borrow_and_update();
        let next = if before == LivenessState::Correct {
            LivenessState::Broken
        } else {
            LivenessState::Correct
        };
        a.set_liveness(next, "leaf-tokio-test");
        assert_eq!(rx.changed().await, next);
    }
}
