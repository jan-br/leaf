//! The `cargo leaf` subcommand skeleton (clap-free, minimal) — the opt-in
//! build-time accelerator + diagnostics CLI (discovery-codegen phase3/02; the
//! deferred `cargo leaf prepare` of the design's "Remaining risks").
//!
//! `cargo leaf <SUB>` is the sqlx-style out-of-band step (the reference's
//! Architecture C): a deterministic build-time pass that emits the checked-in
//! force-link + `ExpectedManifest` + auto-config-plan artifacts the runtime fold
//! would otherwise recompute, plus `doctor`/`metadata` diagnostics. This module
//! owns the PURE, unit-testable core — argument parsing into a typed
//! [`Command`], the dispatch skeleton, and the recommended anti-DCE linker
//! directive — without the filesystem / `cargo metadata` orchestration (a
//! `// NOTE` cross-crate boundary owned by the binary's build step / a future
//! `cargo-leaf` bin).
//!
//! Clap-free on purpose: the macro/build crates pull no CLI dependency, and the
//! parse is a trivial total function over `&[String]` argv — exactly testable.

use std::fmt;

/// The `cargo leaf` subcommands the skeleton recognizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `cargo leaf prepare [--check]` — emit (or, with `--check`, verify) the
    /// checked-in force-link + `ExpectedManifest` + auto-config-plan artifacts.
    Prepare {
        /// `--check`: fail (non-zero) if the checked-in artifact is stale instead
        /// of rewriting it (the sqlx `--check` CI lever).
        check: bool,
    },
    /// `cargo leaf doctor` — print discovered registrations + per-`SourceTag`
    /// row counts (the silent-empty self-check, made human).
    Doctor,
    /// `cargo leaf metadata` — print the config-metadata rollup + any
    /// duplicate-prefix collisions.
    Metadata,
    /// `cargo leaf help` / `--help` / no subcommand — print usage.
    Help,
}

/// A bad-invocation diagnostic from [`parse_args`] (unknown subcommand / flag).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError {
    /// The human-readable explanation.
    pub message: String,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UsageError {}

/// The one-line usage banner `help` / a bad invocation prints.
pub const USAGE: &str = "\
cargo leaf <COMMAND>

Commands:
  prepare [--check]   Emit (or --check) the force-link + ExpectedManifest + auto-config plan
  doctor              List discovered registrations + per-source row counts
  metadata            Print the config-metadata rollup + duplicate-prefix collisions
  help                Print this message";

/// Parse a `cargo leaf` argv tail (the args AFTER `cargo leaf`) into a typed
/// [`Command`]. Clap-free, total, and side-effect-free.
///
/// Accepts the canonical Cargo-subcommand argv shapes: invoked as `cargo leaf X`
/// Cargo passes `["leaf", "X", ..]`, so a leading `"leaf"` token is tolerated and
/// skipped. An empty tail or `help`/`-h`/`--help` is [`Command::Help`].
///
/// # Errors
/// Returns a [`UsageError`] for an unknown subcommand or an unknown flag on a
/// known subcommand (loud, never a silent default).
pub fn parse_args(args: &[String]) -> Result<Command, UsageError> {
    // Tolerate the leading `leaf` token Cargo injects for `cargo leaf …`.
    let mut it = args.iter().map(String::as_str).peekable();
    if it.peek() == Some(&"leaf") {
        it.next();
    }
    let Some(sub) = it.next() else {
        return Ok(Command::Help);
    };
    match sub {
        "help" | "-h" | "--help" => Ok(Command::Help),
        "prepare" => {
            let mut check = false;
            for flag in it {
                match flag {
                    "--check" => check = true,
                    other => {
                        return Err(UsageError {
                            message: format!("unknown flag `{other}` for `prepare`"),
                        });
                    }
                }
            }
            Ok(Command::Prepare { check })
        }
        "doctor" => rest_must_be_empty(it, "doctor").map(|()| Command::Doctor),
        "metadata" => rest_must_be_empty(it, "metadata").map(|()| Command::Metadata),
        other => Err(UsageError {
            message: format!("unknown command `{other}`\n\n{USAGE}"),
        }),
    }
}

/// Guard that a flagless subcommand received no trailing flags.
fn rest_must_be_empty<'a>(
    mut it: impl Iterator<Item = &'a str>,
    sub: &str,
) -> Result<(), UsageError> {
    match it.next() {
        None => Ok(()),
        Some(extra) => Err(UsageError {
            message: format!("`{sub}` takes no arguments (got `{extra}`)"),
        }),
    }
}

/// The recommended ELF anti-DCE linker directive `cargo leaf prepare` emits into
/// the binary crate's `build.rs` (Layer B defense — `--gc-sections` /
/// LLD `start-stop-gc`), per `rust-cross-crate-composition.md` §169.
///
/// On a pre-1.89 toolchain (or LLD) the `linkme` element sections can be GC'd
/// even when the slice symbol is referenced; `-z nostart-stop-gc` keeps them.
/// This is the `cargo::rustc-link-arg` line a build script prints to stdout.
#[must_use]
pub fn anti_dce_link_directive() -> &'static str {
    "cargo::rustc-link-arg=-Wl,-z,nostart-stop-gc"
}

/// A description of what a parsed [`Command`] WOULD do — the dispatch skeleton.
///
/// This is the pure, testable core of the dispatcher: it maps a command to its
/// planned action without performing any IO. The actual orchestration (reading
/// `cargo metadata`, walking the resolved graph, writing `OUT_DIR`, comparing a
/// checked-in artifact) is a cross-crate concern owned by the binary's build
/// step / a future `cargo-leaf` bin, so it stays a `// NOTE` boundary here.
#[must_use]
pub fn describe(command: &Command) -> String {
    match command {
        Command::Prepare { check: false } => {
            "emit force-link shim + ExpectedManifest + auto-config plan".to_string()
        }
        Command::Prepare { check: true } => {
            "verify the checked-in artifact is up to date (fail if stale)".to_string()
        }
        Command::Doctor => "list discovered registrations + per-source row counts".to_string(),
        Command::Metadata => "print the config-metadata rollup + duplicate prefixes".to_string(),
        Command::Help => USAGE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_args_is_help() {
        assert_eq!(parse_args(&[]).unwrap(), Command::Help);
    }

    #[test]
    fn tolerates_the_leading_leaf_token_cargo_injects() {
        // `cargo leaf doctor` reaches the subcommand as `["leaf", "doctor"]`.
        assert_eq!(parse_args(&argv(&["leaf", "doctor"])).unwrap(), Command::Doctor);
        // …and also works without it (direct invocation).
        assert_eq!(parse_args(&argv(&["doctor"])).unwrap(), Command::Doctor);
    }

    #[test]
    fn parses_prepare_with_and_without_check() {
        assert_eq!(
            parse_args(&argv(&["prepare"])).unwrap(),
            Command::Prepare { check: false }
        );
        assert_eq!(
            parse_args(&argv(&["prepare", "--check"])).unwrap(),
            Command::Prepare { check: true }
        );
    }

    #[test]
    fn parses_doctor_and_metadata() {
        assert_eq!(parse_args(&argv(&["doctor"])).unwrap(), Command::Doctor);
        assert_eq!(parse_args(&argv(&["metadata"])).unwrap(), Command::Metadata);
    }

    #[test]
    fn help_flags_are_help() {
        for h in ["help", "-h", "--help"] {
            assert_eq!(parse_args(&argv(&[h])).unwrap(), Command::Help);
        }
    }

    #[test]
    fn an_unknown_command_is_a_loud_usage_error() {
        let err = parse_args(&argv(&["frobnicate"])).expect_err("unknown command must error");
        assert!(err.message.contains("unknown command"), "{}", err.message);
        // The usage banner is included so the error is self-documenting.
        assert!(err.message.contains("Commands:"), "{}", err.message);
    }

    #[test]
    fn an_unknown_prepare_flag_is_a_loud_usage_error() {
        let err = parse_args(&argv(&["prepare", "--nope"])).expect_err("unknown flag must error");
        assert!(err.message.contains("unknown flag"), "{}", err.message);
    }

    #[test]
    fn a_flagless_subcommand_rejects_trailing_args() {
        let err = parse_args(&argv(&["doctor", "extra"])).expect_err("doctor takes no args");
        assert!(err.message.contains("takes no arguments"), "{}", err.message);
    }

    #[test]
    fn anti_dce_directive_is_the_nostart_stop_gc_link_arg() {
        // The Layer-B defense the prepare step recommends for ELF/LLD.
        let d = anti_dce_link_directive();
        assert!(d.starts_with("cargo::rustc-link-arg="), "got: {d}");
        assert!(d.contains("nostart-stop-gc"), "got: {d}");
    }

    #[test]
    fn describe_maps_each_command_to_a_planned_action() {
        assert!(describe(&Command::Prepare { check: false }).contains("force-link"));
        assert!(describe(&Command::Prepare { check: true }).contains("stale"));
        assert!(describe(&Command::Doctor).contains("registrations"));
        assert!(describe(&Command::Metadata).contains("rollup"));
        assert_eq!(describe(&Command::Help), USAGE);
    }
}
