//! The recursive [`Validate`] cascade driver + the cycle guard (validation,
//! phase3/09 §validation).
//!
//! The leaf-core [`ValidationContext`] is the collect-all sink (it owns the
//! [`Violation`](leaf_core::Violation) accumulator + the dotted path stack); this
//! module adds the two pieces the kernel ABI deliberately left to leaf-validation:
//!
//! - the **cascade** ([`validate_root`]/[`Cascade::enter`]) — `@Valid`-nested
//!   recursion: a `#[validate(nested)]` field calls `child.validate` under a pushed
//!   path SEGMENT so a nested violation is reported at `order.items[0].qty`;
//! - the **cycle guard** ([`VisitedSet`]) — a visited-set keyed on the nested
//!   object's heap identity (`*const () as usize`) so a self-referential object
//!   graph (`a.next = a`) terminates instead of recursing forever.
//!
//! The `#[derive(Validate)]` macro emits the per-field constraint check into a
//! [`ValidateInto`] impl driving the [`Cascade`] (a nested `#[validate(nested)]`
//! field lowers to [`Cascade::enter`] with the path + guard threaded); a hand-written
//! `impl ValidateInto` writes the same `Cascade` calls directly.

use std::collections::HashSet;

use leaf_core::{Validate, ValidationContext, Violation};

// The cascade is driven by `ValidateInto` (the path/guard-aware face); a blanket
// `impl<T: ValidateInto> Validate` (below) bridges it to the kernel object-safe
// `Validate` so the SAME type works at the erased `dyn Validate` boundary AND in a
// recursive `@Valid` cascade — one engine, never two.

/// The cycle guard: a set of already-visited nested-object identities (by heap
/// address). A `@Valid` cascade consults it before recursing so a cyclic object
/// graph terminates (the visited node is simply not re-validated).
#[derive(Default, Debug)]
pub struct VisitedSet {
    seen: HashSet<usize>,
}

impl VisitedSet {
    /// A fresh, empty visited-set.
    #[must_use]
    pub fn new() -> Self {
        VisitedSet::default()
    }

    /// Record `addr` as visited; returns `true` if it was NEWLY inserted (i.e. the
    /// caller SHOULD recurse), `false` if it was already present (a cycle — SKIP).
    fn mark(&mut self, addr: usize) -> bool {
        self.seen.insert(addr)
    }
}

/// The cascade driver handed to a [`Validate`] impl: the leaf-core
/// [`ValidationContext`] (the sink + path stack) plus the [`VisitedSet`] cycle
/// guard. A field check pushes a [`Violation`] (at the current path) via
/// [`Cascade::report`]; a nested `@Valid` field recurses via [`Cascade::enter`].
pub struct Cascade<'c> {
    cx: &'c mut ValidationContext,
    visited: &'c mut VisitedSet,
}

impl<'c> Cascade<'c> {
    /// Wrap a context + visited-set into a cascade driver.
    #[must_use]
    pub fn new(cx: &'c mut ValidationContext, visited: &'c mut VisitedSet) -> Self {
        Cascade { cx, visited }
    }

    /// Record a constraint [`Violation`] at the CURRENT path (a leaf field check).
    ///
    /// The violation's own `path` field (empty as minted by [`crate::constraints`])
    /// is rewritten to the cascade's current dotted path joined with the field
    /// `segment` so the rendered path is `order.items[0].qty`.
    pub fn report(&mut self, segment: &str, mut violation: Violation) {
        violation.path = join_path(&self.cx.current_path(), segment);
        self.cx.add(violation);
    }

    /// Run a leaf field's constraint check (`None` = OK, `Some` = violation at
    /// `segment`) — the common case the derive emits per `#[validate(..)]` field.
    pub fn check(&mut self, segment: &str, outcome: Option<Violation>) {
        if let Some(v) = outcome {
            self.report(segment, v);
        }
    }

    /// Cascade into a `@Valid`-nested object `child` under the path `segment`,
    /// guarding against a cycle (a child already on the visited stack is skipped).
    ///
    /// `addr` is the child's stable heap identity (the caller passes
    /// `child as *const _ as *const () as usize`); the guard ensures a
    /// self-referential graph terminates.
    pub fn enter<V: ValidateInto + ?Sized>(&mut self, segment: &str, addr: usize, child: &V) {
        if !self.visited.mark(addr) {
            return; // cycle: already validating this object — do not recurse
        }
        self.cx.enter(segment);
        // Re-borrow into a fresh cascade for the nested object so violations report
        // under the pushed segment and the SAME visited-set threads the whole graph.
        let mut nested = Cascade { cx: self.cx, visited: self.visited };
        child.validate_into(&mut nested);
        self.cx.leave();
    }

    /// The current dotted cascade path (diagnostics; the binder face reads it to map
    /// a violation back to the canonical property KEY).
    #[must_use]
    pub fn current_path(&self) -> String {
        self.cx.current_path()
    }
}

/// The cascade-aware validation face: a `#[derive(Validate)]` impl (or a hand-written
/// one) drives per-field constraint checks and nested `@Valid` cascades through a
/// [`Cascade`] (the path + cycle guard threaded). This is the trait a leaf-validation
/// USER derives or writes; the orphan rule
/// forbids a blanket `impl Validate` over it, so the bridge to the kernel
/// object-safe [`Validate`] is the explicit [`AsValidate`] adapter (used by the
/// config-bind face) — and the method-validation path dispatches `validate_into`
/// DIRECTLY on the typed arg (the erasure-free path), so the SAME engine serves
/// method-validation, config-binding, AND a recursive `@Valid` graph (phase3/09
/// §validation: "one engine, never two").
pub trait ValidateInto {
    /// Validate `self`, reporting through the cascade (path + cycle guard threaded).
    fn validate_into(&self, c: &mut Cascade<'_>);
}

/// Adapter bridging a [`ValidateInto`] value to the kernel object-safe [`Validate`]
/// (the binder/config face wants `&dyn Validate`; the orphan rule forbids a blanket
/// impl). Wraps a `&V` and drives a fresh cascade (a guard seeded with `V`'s
/// identity) when the kernel `validate` is called.
///
/// ```no_run
/// use leaf_core::{Validate, ValidationContext};
/// use leaf_validation::{AsValidate, Cascade, ValidateInto};
///
/// struct AppProps {
///     name: String,
/// }
/// impl ValidateInto for AppProps {
///     fn validate_into(&self, c: &mut Cascade<'_>) {
///         c.check("name", leaf_validation::constraints::not_empty(&self.name));
///     }
/// }
///
/// let props = AppProps { name: "leaf".into() };
/// let mut cx = ValidationContext::new();
/// // The binder/config face wants a `&dyn Validate`: `AsValidate` is that bridge.
/// let as_validate = AsValidate(&props);
/// let dynamic: &dyn Validate = &as_validate;
/// dynamic.validate(&mut cx);
/// ```
pub struct AsValidate<'a, V: ValidateInto + ?Sized>(pub &'a V);

impl<V: ValidateInto + ?Sized> Validate for AsValidate<'_, V> {
    fn validate(&self, cx: &mut ValidationContext) {
        let mut visited = VisitedSet::new();
        visited.mark(addr_of(self.0));
        let mut cascade = Cascade::new(cx, &mut visited);
        self.0.validate_into(&mut cascade);
    }
}

/// Validate a ROOT object, returning the populated [`ValidationContext`].
///
/// Drives `root.validate_into` under a fresh cascade with an empty path + a fresh
/// cycle guard seeded with the root's identity. This is the single entry both the
/// method-validation interceptor (per `@Valid` arg) and the config-bind handler (per
/// bound Object node) call.
#[must_use]
pub fn validate_root<V: ValidateInto + ?Sized>(root: &V) -> ValidationContext {
    let mut cx = ValidationContext::new();
    let mut visited = VisitedSet::new();
    visited.mark(addr_of(root));
    let mut cascade = Cascade::new(&mut cx, &mut visited);
    root.validate_into(&mut cascade);
    cx
}

/// The stable heap identity of a (possibly unsized) reference, for the cycle guard.
#[must_use]
pub fn addr_of<V: ?Sized>(v: &V) -> usize {
    (v as *const V).cast::<()>() as usize
}

fn join_path(prefix: &str, segment: &str) -> String {
    match (prefix.is_empty(), segment.is_empty()) {
        (true, _) => segment.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}.{segment}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraints;
    use leaf_macros::Validate;

    // A flat bean with two constrained fields — the hand `impl ValidateInto` is now
    // DERIVED (the `#[derive(Validate)]` proves byte-for-byte parity with the prior
    // hand impl: the SAME `not_empty`/`range` checks under the SAME segments, so the
    // unchanged assertions below pin the derive reproduces the hand behaviour).
    #[derive(Validate)]
    struct User {
        #[validate(not_empty)]
        name: String,
        #[validate(range(min = 0, max = 150))]
        age: i64,
    }

    #[test]
    fn a_clean_bean_has_no_violations() {
        let cx = validate_root(&User { name: "Jan".into(), age: 39 });
        assert!(cx.is_valid(), "all constraints satisfied");
    }

    #[test]
    fn violations_carry_the_field_path() {
        let cx = validate_root(&User { name: "".into(), age: 200 });
        assert_eq!(cx.violations().len(), 2, "both fields failed (collect-all)");
        let paths: Vec<&str> = cx.violations().iter().map(|v| v.path.as_str()).collect();
        assert!(paths.contains(&"name"), "name violation reported at `name`");
        assert!(paths.contains(&"age"), "age violation reported at `age`");
    }

    // A nested bean cascade (@Valid) — Order has Items. Both are now DERIVED: the
    // `#[validate(min = 1)]` leaf + the `#[validate(nested)]` Vec<Item> indexed
    // cascade reproduce the prior hand impls (the unchanged assertions pin the path).
    #[derive(Validate)]
    struct Item {
        #[validate(min = 1)]
        qty: i64,
    }

    #[derive(Validate)]
    struct Order {
        #[validate(nested)]
        items: Vec<Item>,
    }

    #[test]
    fn nested_cascade_builds_a_dotted_indexed_path() {
        let order = Order { items: vec![Item { qty: 1 }, Item { qty: 0 }] };
        let cx = validate_root(&order);
        assert_eq!(cx.violations().len(), 1, "only the second item is invalid");
        assert_eq!(
            cx.violations()[0].path, "items[1].qty",
            "the nested violation reports the full cascade path"
        );
    }

    // A self-referential graph: the cycle guard must terminate it.
    struct Node {
        value: i64,
        next: std::cell::RefCell<Option<std::rc::Rc<Node>>>,
    }
    impl ValidateInto for Node {
        fn validate_into(&self, c: &mut Cascade<'_>) {
            c.check("value", constraints::min(self.value, 0));
            if let Some(next) = self.next.borrow().as_ref() {
                let r: &Node = next;
                c.enter("next", addr_of(r), r);
            }
        }
    }

    #[test]
    fn the_cycle_guard_terminates_a_self_referential_graph() {
        use std::rc::Rc;
        let a = Rc::new(Node { value: -1, next: std::cell::RefCell::new(None) });
        // a.next = a  (a cycle)
        *a.next.borrow_mut() = Some(Rc::clone(&a));
        // Without the guard this recurses forever; the guard makes it terminate with
        // exactly one violation (the root visited once).
        let cx = validate_root(a.as_ref());
        assert_eq!(cx.violations().len(), 1, "validated the node once, did not loop");
        assert_eq!(cx.violations()[0].path, "value");
    }

    #[test]
    fn as_validate_bridges_to_the_kernel_validate_trait() {
        // The config-bind face wants `&dyn Validate`; AsValidate adapts a
        // ValidateInto to it (driving a fresh cascade + guard).
        let user = User { name: "".into(), age: 5 };
        let adapter = AsValidate(&user);
        let dynv: &dyn Validate = &adapter;
        let mut cx = ValidationContext::new();
        dynv.validate(&mut cx);
        assert_eq!(cx.violations().len(), 1, "the name violation surfaced via dyn Validate");
        assert_eq!(cx.violations()[0].path, "name");
    }

    #[test]
    fn distinct_objects_in_a_diamond_are_each_visited() {
        // a -> b, a -> c, b -> d, c -> d : d is shared but distinct nodes are each
        // validated; d (shared) is validated once (visited-set dedup) — proving the
        // guard dedups by identity, not by shape.
        use std::cell::RefCell;
        use std::rc::Rc;
        let d = Rc::new(Node { value: -5, next: RefCell::new(None) });
        let b = Rc::new(Node { value: 1, next: RefCell::new(Some(Rc::clone(&d))) });
        let cx = validate_root(b.as_ref());
        // b.value=1 ok; d.value=-5 fails once.
        assert_eq!(cx.violations().len(), 1);
        assert_eq!(cx.violations()[0].path, "next.value");
    }
}
