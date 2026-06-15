//! The ONE diagnostic spine: [`LeafError`], [`Diagnostic`], [`FailureAnalyzer`].
//!
//! Realizes ADR-12 (error-model) and TOOLKIT.md: EVERY framework error,
//! regardless of where it is born, is one node in a uniform causal chain that
//! carries (what was being assembled / what it needed / what was expected /
//! what was actually found incl. candidates-considered / where), and EVERY
//! user-facing surfacing of that chain goes through ONE [`Diagnostic`] renderer.
//!
//! [`LeafError`] is a STRUCT, not a flat enum: a typed [`ErrorKind`] (the closed
//! taxonomy + one open `Integration` arm) + a `chain: Box<[Cause]>` (the
//! explicit causal narrative, Spring's nested-exception intent as DATA not
//! unwinding) + an [`Origin`] + a [`Severity`]. It implements
//! [`std::error::Error`] with a real `source()` walk over the chain — so it
//! composes with `?`, `anyhow`, and `tracing`'s error field — but its OWN
//! richness lives in `kind`/`chain`, never in a backtrace.
//!
//! [`FailureAnalyzer`] is NOT a parallel error system: it is the
//! rendering-policy layer over [`LeafError`] (Spring's `FailureAnalysis` shape).
//!
//! Scope note (UNIT 1 — bedrock): the spine shape
//! (`LeafError`/`ErrorKind`/`Cause`/`CauseDetail`/`Origin`/`Severity`/
//! `Diagnostic`/`FailureAnalyzer`) is complete, and the richest [`CauseDetail`]
//! payloads from ADR-12 (`Candidates{considered, trace}`, `AssemblyAt{bean,
//! edge}`) have LANDED here over real [`BeanId`]/[`InjectionEdge`]/
//! [`CandidateInfo`]/[`NarrowStep`] types — the enum stays `#[non_exhaustive]`
//! so later units may still extend it without an ABI break. leaf-core owes no
//! open work-item in this module: the one remaining design boundary — a
//! serde-DERIVED `RenderStyle::StructuredJson` schema as a stable tooling
//! contract — is DELIBERATELY deferred (ADR-12; TOOLKIT.md) and kept out of the
//! serde-free kernel by charter §6; see the `NOTE` at the `StructuredJson` arm.

use std::fmt;

use crate::identity::{BeanId, ContractId};

/// Severity of a [`LeafError`] node.
///
/// `Fatal` aborts; `Warn` is the recognized degrade-and-warn category (e.g. a
/// banner-render failure, or a `Lenient` startup-validation downgrade); `Info`
/// is a non-failing diagnostic record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Severity {
    /// Aborts the affected phase.
    #[default]
    Fatal,
    /// Degrade-and-warn: surfaced but does not abort (e.g. banner failure).
    Warn,
    /// Informational diagnostic record.
    Info,
}

/// The unified "where" of an error node — shared with the config/registry ADRs.
///
/// The compile-time [`Origin::Span`] and config-provenance [`Origin::File`] arms
/// are PLAIN DATA: leaf-core defines the shape (no `proc_macro` dependency); the
/// `#[component]`/`#[value]` macros and the config-data loaders fill them later
/// with `file!()`/`line!()`/`column!()` and the parsed file path+line.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Origin {
    /// No location information available.
    #[default]
    Unknown,
    /// A native (link-time) registration; optionally names the source crate.
    Native {
        /// The contributing crate name, if known (anti-DCE provenance).
        crate_name: Option<&'static str>,
    },
    /// A test double / programmatically-installed contribution.
    TestDouble,
    /// A compile-time source location (the macro fills this from
    /// `file!()`/`line!()`/`column!()`). Plain data — no `proc_macro` dep.
    Span {
        /// The source file path (`file!()`).
        file: &'static str,
        /// The 1-based source line (`line!()`).
        line: u32,
        /// The 1-based source column (`column!()`).
        col: u32,
    },
    /// A config-file provenance (the config-data loader fills this from the
    /// parsed property's file path + line). Plain data — no interned id needed.
    File {
        /// The config file path (e.g. `application.yaml`).
        path: &'static str,
        /// The 1-based line within the config file.
        line: u32,
    },
}

/// The CLOSED core error taxonomy + ONE open data arm.
///
/// `#[non_exhaustive]` so adding a core variant is a minor-but-careful change;
/// the open [`ErrorKind::Integration`] arm means integration crates (e.g.
/// DataAccessError translation) extend the taxonomy BY DATA, keyed by
/// [`ContractId`], without ever widening the core ABI.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    // ── Tier 2: startup-wiring (richest cross-bean tier) ──
    /// No provider for the requested type/name.
    NoSuchBean,
    /// More than one candidate and no winner could be chosen.
    NoUniqueBean,
    /// A `dyn`-advised bean injected at a concrete type (advice would be lost).
    AdvisedConcreteInjection,
    /// A bean's declared scope is incompatible with an injection point's scope.
    ScopeMismatch,
    /// A constructor-injection cycle (the only break is a deferral edge).
    CircularDependency,
    /// A `@DependsOn`-declared ordering cycle.
    DependsOnCycle,
    // ── Tier 2: config / validation ──
    /// A required `${...}`/`@Value` placeholder could not be resolved.
    UnresolvedValue,
    /// A type conversion failed.
    ConvertError,
    /// `@ConfigurationProperties` binding failed.
    BindError,
    /// A bean-validation (JSR-style) constraint was violated.
    ValidationError,
    /// A profile activation/requirement error.
    ProfileError,
    /// A `@Conditional` evaluation error (NOT a silent condition-not-met).
    ConditionError,
    // ── Tier 1: build / freeze ──
    /// A crate in the expected manifest contributed zero rows (DCE-dropped).
    AntiDce,
    /// Two distinct canonical paths hashed to the same [`ContractId`].
    ContractCollision,
    /// An auto-config ordering cycle / typo'd before/after/exclude.
    AutoConfigOrdering,
    /// A `cargo leaf prepare` plan is stale relative to the sealed registry.
    PlanStale,
    // ── Tier 3: runtime / value-phase ──
    /// A constructor body / async task failed.
    ConstructionFailed,
    /// A configuration-IO failure.
    ConfigIo,
    /// A build/run was cancelled.
    Cancelled,
    /// A message code could not be resolved by any `MessageSource`
    /// (expr-i18n-resources phase3/11).
    NoSuchMessage,
    // ── the ONE open, by-data-extensible arm ──
    /// Integration-contributed error kind, keyed by stable [`ContractId`].
    Integration {
        /// The integration's stable taxonomy id.
        kind_id: ContractId,
    },
}

impl ErrorKind {
    /// A short, stable, machine-friendly slug for this kind (rendering/tests).
    #[must_use]
    pub fn slug(&self) -> &'static str {
        match self {
            ErrorKind::NoSuchBean => "no-such-bean",
            ErrorKind::NoUniqueBean => "no-unique-bean",
            ErrorKind::AdvisedConcreteInjection => "advised-concrete-injection",
            ErrorKind::ScopeMismatch => "scope-mismatch",
            ErrorKind::CircularDependency => "circular-dependency",
            ErrorKind::DependsOnCycle => "depends-on-cycle",
            ErrorKind::UnresolvedValue => "unresolved-value",
            ErrorKind::ConvertError => "convert-error",
            ErrorKind::BindError => "bind-error",
            ErrorKind::ValidationError => "validation-error",
            ErrorKind::ProfileError => "profile-error",
            ErrorKind::ConditionError => "condition-error",
            ErrorKind::AntiDce => "anti-dce",
            ErrorKind::ContractCollision => "contract-collision",
            ErrorKind::AutoConfigOrdering => "auto-config-ordering",
            ErrorKind::PlanStale => "plan-stale",
            ErrorKind::ConstructionFailed => "construction-failed",
            ErrorKind::ConfigIo => "config-io",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::NoSuchMessage => "no-such-message",
            ErrorKind::Integration { .. } => "integration",
        }
    }

    /// The conventional process exit code for this kind.
    ///
    /// Folds the Spring `ExitCodeExceptionMapper` into ONE story over the typed
    /// taxonomy (ADR-12): there is no separate exit-code SPI, only this fn plus
    /// a single `App`-level override hook owned by leaf-boot.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            // A clean cancellation is not a hard failure.
            ErrorKind::Cancelled => 0,
            // Everything else is a generic failure code by default; specific
            // codes (e.g. port-in-use) are contributed via FailureAnalyzers /
            // the App-level override hook in leaf-boot.
            _ => 1,
        }
    }
}

/// One candidate that was CONSIDERED while resolving an ambiguous injection
/// point (the `NoUniqueBean` rich-context payload, ADR-12 §1.7).
///
/// A small owned snapshot lifted off the injection `Cand` read-view at the
/// failure point (the error node owns its narrative, never a borrow into a
/// frozen registry that may outlive the diagnostic). `ty` is a human type-name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CandidateInfo {
    /// The candidate bean's canonical name.
    pub name: String,
    /// The candidate's rendered type-name.
    pub ty: String,
}

impl CandidateInfo {
    /// Build a candidate snapshot from its name + rendered type-name.
    #[must_use]
    pub fn new(name: impl Into<String>, ty: impl Into<String>) -> Self {
        CandidateInfo { name: name.into(), ty: ty.into() }
    }
}

/// One recorded step of the Selector's narrowing fold (ADR-12 §1.7): a layer
/// name + the working-set size before and after that layer ran.
///
/// This is the owned, diagnostic projection of the injection `Trace`'s
/// `(layer, in_len, out_len)` tuples — so a `NoUniqueBean` error can replay
/// "primary_promote: 3 → 2, name_match: 2 → 2" without re-running resolution.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NarrowStep {
    /// The narrowing layer's stable name.
    pub layer: &'static str,
    /// The working-set size entering the layer.
    pub in_len: usize,
    /// The working-set size leaving the layer.
    pub out_len: usize,
}

/// The injection edge an assembly failure occurred at (the `AssemblyAt` payload):
/// the declared injection-point name + the rendered required type-name.
///
/// Owned (a `String` type-name, not a `TypeId`) so the rendered diagnostic is
/// self-contained and human-legible.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InjectionEdge {
    /// The declared param/field name of the injection point.
    pub point: String,
    /// The rendered required type-name at that point.
    pub required_ty: String,
}

impl InjectionEdge {
    /// Build an injection edge from a point name + the rendered required type.
    #[must_use]
    pub fn new(point: impl Into<String>, required_ty: impl Into<String>) -> Self {
        InjectionEdge { point: point.into(), required_ty: required_ty.into() }
    }
}

/// The "what was actually found" payload of one [`Cause`] node.
///
/// `#[non_exhaustive]`: integration crates / later additive units may extend it.
/// The `Candidates`/`AssemblyAt` arms carry the registry/injection rich context.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CauseDetail {
    /// A free-form narrative line.
    Plain(String),
    /// "expected but missing" — the NoSuchBean / anti-DCE shape.
    ///
    /// `ty` is a human type-name placeholder until the registry unit lands a
    /// `TypeKey`; `missing_source` names the crate expected to have provided it.
    Expected {
        /// The expected type, rendered as a name (placeholder for `TypeKey`).
        ty: String,
        /// The source crate expected to contribute it, if known.
        missing_source: Option<&'static str>,
    },
    /// The `NoUniqueBean` rich context (ADR-12 §1.7): every candidate that was
    /// considered + the Selector's narrowing-fold trace that failed to choose.
    Candidates {
        /// The candidates considered at the ambiguous point.
        considered: Box<[CandidateInfo]>,
        /// The Selector's recorded narrowing steps (the traced fold).
        trace: Box<[NarrowStep]>,
    },
    /// The assembly site of a wiring failure: which bean was being assembled and
    /// across which injection edge.
    AssemblyAt {
        /// The dense slot id of the bean being assembled.
        bean: BeanId,
        /// The injection edge the failure occurred at.
        edge: InjectionEdge,
    },
}

impl fmt::Display for CauseDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CauseDetail::Plain(s) => f.write_str(s),
            CauseDetail::Expected { ty, missing_source } => {
                write!(f, "expected `{ty}`")?;
                if let Some(src) = missing_source {
                    write!(f, " (source crate `{src}`)")?;
                }
                Ok(())
            }
            CauseDetail::Candidates { considered, trace } => {
                write!(f, "{} candidates considered: [", considered.len())?;
                for (i, c) in considered.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{} (`{}`)", c.name, c.ty)?;
                }
                f.write_str("]")?;
                if !trace.is_empty() {
                    f.write_str("; narrowing: ")?;
                    for (i, s) in trace.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{} ({}→{})", s.layer, s.in_len, s.out_len)?;
                    }
                }
                Ok(())
            }
            CauseDetail::AssemblyAt { bean, edge } => {
                write!(
                    f,
                    "assembling {bean:?} at injection point `{}` (required `{}`)",
                    edge.point, edge.required_ty
                )
            }
        }
    }
}

/// One node of the explicit causal narrative.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cause {
    /// What was being done when this node arose (e.g. "resolving dependency").
    pub what: &'static str,
    /// What was actually found / the payload of this node.
    pub detail: CauseDetail,
    /// Where this node arose.
    pub origin: Origin,
}

impl Cause {
    /// A free-form narrative cause node.
    #[must_use]
    pub fn plain(what: &'static str, detail: impl Into<String>) -> Self {
        Cause {
            what,
            detail: CauseDetail::Plain(detail.into()),
            origin: Origin::Unknown,
        }
    }

    /// Attach an [`Origin`] to this cause node (builder style).
    #[must_use]
    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }
}

/// THE one causal-chain error type (ADR-12). See the module docs.
///
/// `Clone`/`PartialEq`/`Eq` are implemented by hand (not derived) so they ignore
/// the private lazily-built `source()` view cache — the cache is implementation
/// detail and never part of error identity.
pub struct LeafError {
    /// The typed taxonomy (closed core + one open `Integration` arm).
    pub kind: ErrorKind,
    /// The explicit causal narrative (Spring nested-exception intent, as data).
    pub chain: Box<[Cause]>,
    /// Where the error was born.
    pub origin: Origin,
    /// Severity (`Fatal`/`Warn`/`Info`).
    pub mode: Severity,
    /// Lazily-built linked view backing [`std::error::Error::source`]. Not part
    /// of error identity; reset on clone / chain mutation.
    source_cache: once_cell::sync::OnceCell<Option<Box<CauseError>>>,
}

impl Clone for LeafError {
    fn clone(&self) -> Self {
        LeafError {
            kind: self.kind,
            chain: self.chain.clone(),
            origin: self.origin,
            mode: self.mode,
            // The cache is rebuilt lazily; never carried across a clone.
            source_cache: once_cell::sync::OnceCell::new(),
        }
    }
}

impl PartialEq for LeafError {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.chain == other.chain
            && self.origin == other.origin
            && self.mode == other.mode
    }
}

impl Eq for LeafError {}

impl LeafError {
    /// A `Fatal` error of `kind` with no causal chain yet.
    #[must_use]
    pub fn new(kind: ErrorKind) -> Self {
        LeafError {
            kind,
            chain: Box::new([]),
            origin: Origin::Unknown,
            mode: Severity::Fatal,
            source_cache: once_cell::sync::OnceCell::new(),
        }
    }

    /// Push a [`Cause`] node onto the chain (builder style).
    ///
    /// Nodes are stored in causal order (each appended node is a deeper cause);
    /// [`source`](LeafError::source) walks them from the first downward.
    #[must_use]
    pub fn caused_by(mut self, cause: Cause) -> Self {
        let mut v = self.chain.into_vec();
        v.push(cause);
        self.chain = v.into_boxed_slice();
        // A mutated chain invalidates any built view.
        self.source_cache = once_cell::sync::OnceCell::new();
        self
    }

    /// Set the error's own [`Origin`] (builder style).
    #[must_use]
    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    /// Set the [`Severity`] (builder style); e.g. downgrade to `Warn`.
    #[must_use]
    pub fn with_severity(mut self, mode: Severity) -> Self {
        self.mode = mode;
        self
    }

    /// The conventional exit code for this error (delegates to [`ErrorKind`]).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.kind.exit_code()
    }
}

impl fmt::Display for LeafError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind.slug())?;
        if let Some(first) = self.chain.first() {
            write!(f, ": {} — {}", first.what, first.detail)?;
        }
        Ok(())
    }
}

impl fmt::Debug for LeafError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeafError")
            .field("kind", &self.kind)
            .field("mode", &self.mode)
            .field("origin", &self.origin)
            .field("chain", &self.chain)
            .finish()
    }
}

/// Adapter so a single [`Cause`] node can participate in the
/// [`std::error::Error::source`] walk (the ecosystem-interop facade).
#[derive(Debug)]
struct CauseError {
    what: &'static str,
    detail: String,
    /// The next deeper cause in the chain, if any.
    next: Option<Box<CauseError>>,
}

impl fmt::Display for CauseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.what, self.detail)
    }
}

impl std::error::Error for CauseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.next
            .as_deref()
            .map(|c| c as &(dyn std::error::Error + 'static))
    }
}

impl std::error::Error for LeafError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // Interop facade ONLY (?, anyhow, tracing) — the genuine richness lives
        // in `kind`/`chain`. We materialize a linked CauseError view lazily into
        // a cached cell so the borrow outlives this call.
        self.source_view().map(|c| c as &(dyn std::error::Error + 'static))
    }
}

impl LeafError {
    /// Build (and memoize) the linked [`CauseError`] view used by `source()`.
    ///
    /// `source()` must hand out a reference that outlives the call, so the view
    /// is built once on demand and stored in a `OnceCell` inside `self`.
    fn source_view(&self) -> Option<&CauseError> {
        self.source_cache
            .get_or_init(|| build_cause_view(&self.chain))
            .as_ref()
            .map(std::convert::AsRef::as_ref)
    }
}

fn build_cause_view(chain: &[Cause]) -> Option<Box<CauseError>> {
    // Fold the chain from the deepest node back to the first so each links to
    // the next deeper one.
    let mut next: Option<Box<CauseError>> = None;
    for cause in chain.iter().rev() {
        next = Some(Box::new(CauseError {
            what: cause.what,
            detail: cause.detail.to_string(),
            next: next.take(),
        }));
    }
    next
}

/// How a [`Diagnostic`] should render.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RenderStyle {
    /// Human-readable prose (the default reporter, to stderr).
    #[default]
    Human,
    /// Structured machine-parseable output (serde, future tooling contract).
    StructuredJson,
    /// `tracing`-field oriented rendering (the leaf-tracing bridge).
    TracingFields,
}

/// The ONE renderer trait. Implemented once for [`LeafError`] (and later for
/// `AssemblyReport`/`ConditionReport`), it walks the causal chain and overlays
/// a matched [`FailureAnalysis`] when an analyzer fired.
pub trait Diagnostic {
    /// Render `self` into `w` in the given [`RenderStyle`].
    ///
    /// # Errors
    /// Propagates any write error from the underlying [`fmt::Write`] sink.
    fn render(&self, w: &mut dyn fmt::Write, style: RenderStyle) -> fmt::Result;

    /// Convenience: render to an owned `String`.
    #[must_use]
    fn render_to_string(&self, style: RenderStyle) -> String {
        let mut s = String::new();
        // Writing to a String is infallible.
        let _ = self.render(&mut s, style);
        s
    }
}

impl Diagnostic for LeafError {
    fn render(&self, w: &mut dyn fmt::Write, style: RenderStyle) -> fmt::Result {
        match style {
            RenderStyle::Human => {
                writeln!(w, "{}: {}", severity_label(self.mode), self.kind.slug())?;
                for (depth, cause) in self.chain.iter().enumerate() {
                    let indent = "  ".repeat(depth + 1);
                    writeln!(w, "{indent}- {}: {}", cause.what, cause.detail)?;
                }
                Ok(())
            }
            RenderStyle::StructuredJson => {
                // A minimal, hand-rolled JSON object so the core stays
                // serde-free (charter §6: core depends on no integration).
                // NOTE: a serde-DERIVED stable schema is a deliberately-deferred
                // tooling contract (ADR-12 §"deferred"; TOOLKIT.md DX line) —
                // gated on measured `leaf doctor`/CI demand and kept OUT of the
                // serde-free kernel by charter §6, not an open leaf-core task.
                // What IS guaranteed here is a well-formed, escaped JSON object
                // (a parseable boundary), pinned by
                // `diagnostic_structured_json_escapes_significant_characters`.
                write!(w, "{{\"kind\":\"{}\",", self.kind.slug())?;
                write!(w, "\"severity\":\"{}\",", severity_label(self.mode))?;
                write!(w, "\"causes\":[")?;
                for (i, cause) in self.chain.iter().enumerate() {
                    if i > 0 {
                        write!(w, ",")?;
                    }
                    write!(
                        w,
                        "{{\"what\":{:?},\"detail\":{:?}}}",
                        cause.what,
                        cause.detail.to_string()
                    )?;
                }
                write!(w, "]}}")
            }
            RenderStyle::TracingFields => {
                write!(w, "kind={} severity={}", self.kind.slug(), severity_label(self.mode))?;
                for cause in self.chain.iter() {
                    write!(w, " cause=\"{}: {}\"", cause.what, cause.detail)?;
                }
                Ok(())
            }
        }
    }
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Fatal => "error",
        Severity::Warn => "warning",
        Severity::Info => "info",
    }
}

/// Spring's `FailureAnalysis` shape: teachable prose + an action + an optional
/// root cause. Produced by a [`FailureAnalyzer`] as a rendering overlay over
/// the always-present [`LeafError`] chain.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FailureAnalysis {
    /// What went wrong, in teachable prose.
    pub description: String,
    /// What the user should do about it.
    pub action: String,
    /// An optional root-cause node lifted from the chain.
    pub cause: Option<Cause>,
}

/// Context handed to a [`FailureAnalyzer`] by EXPLICIT construction (never
/// injection): later units thread `Env`, the partial registry, and the frozen
/// `ConditionReport` through this.
///
/// Scope note (UNIT 1): those references are owned by later units, so this is a
/// minimal forward-compatible placeholder. It is `#[non_exhaustive]` so fields
/// can be added without breaking analyzer impls that construct it via
/// [`AnalysisCtx::empty`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default)]
pub struct AnalysisCtx {}

impl AnalysisCtx {
    /// An empty context (no container state available yet).
    #[must_use]
    pub fn empty() -> Self {
        AnalysisCtx {}
    }
}

/// The FailureAnalyzer SPI (ADR-12 §1.7/§2.9): the rendering-POLICY layer over
/// the one [`LeafError`], NOT a parallel error system.
///
/// Analyzers match on [`ErrorKind`] / downcast over the typed chain (not
/// Spring's reflective generic match) and return teachable prose. They are
/// collected through the codegen-boundary `FAILURE_ANALYZERS` linkme slice
/// (owned by the discovery unit), `@Order`-sorted, first-non-`None` wins.
pub trait FailureAnalyzer: Send + Sync {
    /// Analyze `err`; return a [`FailureAnalysis`] iff this analyzer applies.
    fn analyze(&self, err: &LeafError, ctx: &AnalysisCtx) -> Option<FailureAnalysis>;
}

/// Run a slice of analyzers in order, returning the first non-`None` analysis.
///
/// (The built-in analyzers and the `FAILURE_ANALYZERS` linkme slice itself are
/// owned by the discovery/bootstrap units; this is the bedrock chain-walk.)
#[must_use]
pub fn analyze_first(
    analyzers: &[&dyn FailureAnalyzer],
    err: &LeafError,
    ctx: &AnalysisCtx,
) -> Option<FailureAnalysis> {
    analyzers.iter().find_map(|a| a.analyze(err, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_error_display_summarizes_kind_and_first_cause() {
        let e = LeafError::new(ErrorKind::NoSuchBean)
            .caused_by(Cause::plain("resolving bean", "no provider for `UserService`"));
        let s = e.to_string();
        assert!(s.contains("no-such-bean"), "got: {s}");
        assert!(s.contains("no provider for `UserService`"), "got: {s}");
    }

    #[test]
    fn source_walks_the_causal_chain_not_a_backtrace() {
        use std::error::Error;
        let e = LeafError::new(ErrorKind::ConstructionFailed)
            .caused_by(Cause::plain("constructing `A`", "depends on `B`"))
            .caused_by(Cause::plain("constructing `B`", "io error opening config"));

        // First source = first (shallowest) cause; then it chains deeper.
        let src1 = e.source().expect("has a source");
        assert!(src1.to_string().contains("constructing `A`"), "got: {src1}");
        let src2 = src1.source().expect("has a deeper source");
        assert!(src2.to_string().contains("constructing `B`"), "got: {src2}");
        assert!(src2.source().is_none(), "chain terminates");
    }

    #[test]
    fn source_is_none_for_an_empty_chain() {
        use std::error::Error;
        let e = LeafError::new(ErrorKind::Cancelled);
        assert!(e.source().is_none());
    }

    #[test]
    fn composes_with_question_mark_and_box_dyn_error() {
        fn fallible() -> Result<(), Box<dyn std::error::Error>> {
            Err(LeafError::new(ErrorKind::BindError)
                .caused_by(Cause::plain("binding", "bad value")))?;
            Ok(())
        }
        let err = fallible().expect_err("propagates");
        assert!(err.to_string().contains("bind-error"));
    }

    #[test]
    fn integration_arm_is_keyed_by_contract_id() {
        let id = ContractId::of("leaf_data::DataAccessError");
        let e = LeafError::new(ErrorKind::Integration { kind_id: id });
        match e.kind {
            ErrorKind::Integration { kind_id } => assert_eq!(kind_id, id),
            other => panic!("expected Integration arm, got {other:?}"),
        }
    }

    #[test]
    fn exit_code_folds_into_error_kind() {
        assert_eq!(LeafError::new(ErrorKind::Cancelled).exit_code(), 0);
        assert_eq!(LeafError::new(ErrorKind::NoSuchBean).exit_code(), 1);
    }

    #[test]
    fn diagnostic_human_render_walks_the_chain() {
        let e = LeafError::new(ErrorKind::NoUniqueBean)
            .caused_by(Cause::plain("resolving", "two candidates"))
            .caused_by(Cause::plain("narrowing", "no @Primary"));
        let out = e.render_to_string(RenderStyle::Human);
        assert!(out.contains("error: no-unique-bean"), "got: {out}");
        assert!(out.contains("resolving: two candidates"), "got: {out}");
        assert!(out.contains("narrowing: no @Primary"), "got: {out}");
    }

    #[test]
    fn diagnostic_warn_severity_renders_as_warning() {
        let e = LeafError::new(ErrorKind::ConfigIo)
            .with_severity(Severity::Warn)
            .caused_by(Cause::plain("banner", "figlet font missing"));
        let out = e.render_to_string(RenderStyle::Human);
        assert!(out.contains("warning: config-io"), "got: {out}");
    }

    #[test]
    fn diagnostic_structured_json_is_parseable_shape() {
        let e = LeafError::new(ErrorKind::NoSuchBean)
            .caused_by(Cause::plain("resolving", "missing"));
        let out = e.render_to_string(RenderStyle::StructuredJson);
        assert!(out.starts_with('{') && out.ends_with('}'), "got: {out}");
        assert!(out.contains("\"kind\":\"no-such-bean\""), "got: {out}");
        assert!(out.contains("\"causes\":["), "got: {out}");
    }

    // The hand-rolled, serde-free JSON is a documented BOUNDARY (see the
    // module-level NOTE / ADR-12): it is deliberately not a serde-derived
    // schema, but it MUST stay well-formed so downstream tooling can parse it.
    // Pin the load-bearing invariant: detail text carrying JSON-significant
    // characters (`"`, `\`, control bytes) is escaped, never emitted raw.
    #[test]
    fn diagnostic_structured_json_escapes_significant_characters() {
        let e = LeafError::new(ErrorKind::ConfigIo).caused_by(Cause::plain(
            "parsing",
            "bad value \"x\" at path C:\\tmp\nline2",
        ));
        let out = e.render_to_string(RenderStyle::StructuredJson);
        // The raw, UNescaped payload must NOT appear verbatim (that would break
        // the surrounding JSON object the boundary promises).
        assert!(
            !out.contains("\"x\" at path C:\\tmp\nline2"),
            "raw unescaped payload leaked into JSON: {out}"
        );
        // The escaped forms MUST be present: `\"`, `\\`, `\n`.
        assert!(out.contains("\\\"x\\\""), "quote not escaped: {out}");
        assert!(out.contains("C:\\\\tmp"), "backslash not escaped: {out}");
        assert!(out.contains("\\n"), "newline not escaped: {out}");
        // And the object braces stay balanced + intact at the boundary.
        assert!(out.starts_with('{') && out.ends_with('}'), "got: {out}");
        assert_eq!(
            out.matches('{').count(),
            out.matches('}').count(),
            "unbalanced braces: {out}"
        );
    }

    #[test]
    fn expected_cause_detail_renders_type_and_source() {
        let cause = Cause {
            what: "auto-discovery",
            detail: CauseDetail::Expected {
                ty: "RedisClient".into(),
                missing_source: Some("leaf-redis"),
            },
            origin: Origin::Native { crate_name: Some("leaf-redis") },
        };
        let e = LeafError::new(ErrorKind::AntiDce).caused_by(cause);
        let out = e.render_to_string(RenderStyle::Human);
        assert!(out.contains("expected `RedisClient`"), "got: {out}");
        assert!(out.contains("source crate `leaf-redis`"), "got: {out}");
    }

    struct NoSuchBeanAnalyzer;
    impl FailureAnalyzer for NoSuchBeanAnalyzer {
        fn analyze(&self, err: &LeafError, _ctx: &AnalysisCtx) -> Option<FailureAnalysis> {
            if err.kind == ErrorKind::NoSuchBean {
                Some(FailureAnalysis {
                    description: "A required bean was not found.".into(),
                    action: "Define it or add the missing starter.".into(),
                    cause: err.chain.first().cloned(),
                })
            } else {
                None
            }
        }
    }

    struct NeverAnalyzer;
    impl FailureAnalyzer for NeverAnalyzer {
        fn analyze(&self, _err: &LeafError, _ctx: &AnalysisCtx) -> Option<FailureAnalysis> {
            None
        }
    }

    #[test]
    fn failure_analyzer_first_non_none_wins() {
        let never = NeverAnalyzer;
        let matcher = NoSuchBeanAnalyzer;
        let analyzers: [&dyn FailureAnalyzer; 2] = [&never, &matcher];
        let ctx = AnalysisCtx::empty();

        let e = LeafError::new(ErrorKind::NoSuchBean)
            .caused_by(Cause::plain("resolving", "no provider"));
        let analysis = analyze_first(&analyzers, &e, &ctx).expect("matched");
        assert_eq!(analysis.description, "A required bean was not found.");
        assert!(analysis.cause.is_some());

        // A non-matching error yields no analysis.
        let other = LeafError::new(ErrorKind::ConvertError);
        assert!(analyze_first(&analyzers, &other, &ctx).is_none());
    }

    #[test]
    fn leaf_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LeafError>();
    }

    // ── Origin::Span / Origin::File plain-data variants (closure 2) ──

    #[test]
    fn origin_span_and_file_are_plain_data_variants() {
        let span = Origin::Span { file: "src/app.rs", line: 12, col: 5 };
        match span {
            Origin::Span { file, line, col } => {
                assert_eq!(file, "src/app.rs");
                assert_eq!(line, 12);
                assert_eq!(col, 5);
            }
            other => panic!("expected Span, got {other:?}"),
        }
        let file = Origin::File { path: "application.yaml", line: 7 };
        match file {
            Origin::File { path, line } => {
                assert_eq!(path, "application.yaml");
                assert_eq!(line, 7);
            }
            other => panic!("expected File, got {other:?}"),
        }
        // They are Copy (the whole Origin enum stays Copy).
        let s2 = span;
        assert_eq!(span, s2);
    }

    // ── CauseDetail::Candidates / AssemblyAt rich payloads (closure 1) ──

    #[test]
    fn candidates_cause_detail_carries_considered_and_trace() {
        let considered = vec![
            CandidateInfo::new("primaryDs", "DataSource"),
            CandidateInfo::new("backupDs", "DataSource"),
        ];
        let trace = vec![
            NarrowStep { layer: "primary_promote", in_len: 2, out_len: 2 },
            NarrowStep { layer: "name_match", in_len: 2, out_len: 2 },
        ];
        let detail = CauseDetail::Candidates {
            considered: considered.into_boxed_slice(),
            trace: trace.into_boxed_slice(),
        };
        // Renders the considered candidate names (the §1.7 rich context).
        let rendered = detail.to_string();
        assert!(rendered.contains("primaryDs"), "got: {rendered}");
        assert!(rendered.contains("backupDs"), "got: {rendered}");
    }

    #[test]
    fn assembly_at_cause_detail_carries_bean_and_edge() {
        use crate::identity::BeanId;
        let detail = CauseDetail::AssemblyAt {
            bean: BeanId(3),
            edge: InjectionEdge::new("dataSource", "DataSource"),
        };
        let rendered = detail.to_string();
        assert!(rendered.contains("dataSource"), "got: {rendered}");
    }

    #[test]
    fn no_unique_bean_error_can_carry_the_considered_candidate_list() {
        // The NoUniqueBean error should be expressible with the rich Candidates
        // payload (the considered list + the narrowing trace).
        let cause = Cause {
            what: "resolving injection point",
            detail: CauseDetail::Candidates {
                considered: vec![
                    CandidateInfo::new("a", "Svc"),
                    CandidateInfo::new("b", "Svc"),
                ]
                .into_boxed_slice(),
                trace: vec![NarrowStep { layer: "primary_promote", in_len: 2, out_len: 2 }]
                    .into_boxed_slice(),
            },
            origin: Origin::Unknown,
        };
        let e = LeafError::new(ErrorKind::NoUniqueBean).caused_by(cause);
        let out = e.render_to_string(RenderStyle::Human);
        assert!(out.contains("no-unique-bean"), "got: {out}");
        assert!(out.contains('a') && out.contains('b'), "got: {out}");
    }
}
