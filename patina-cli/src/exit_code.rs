//! The CLI's formalized process exit codes (REQ-022).
//!
//! Every terminal CLI state maps to exactly one of these codes. The
//! contract is enforced in one place: subcommands return an [`ExitCode`]
//! (or an `anyhow::Error` carrying a [`patina_core::EngineError`]), and
//! [`crate::cli::resolve_exit_code`] is the single funnel that turns either
//! into a process exit status.
//!
//! | Code | Meaning                                                       |
//! |------|---------------------------------------------------------------|
//! | 0    | Success.                                                      |
//! | 1    | Generic error (config parse, IO, undefined variable, version mismatch, missing prior apply, unresolved shell, …). |
//! | 2    | A `must_succeed` `pre_apply` hook failed; apply aborted before any file operation. |
//! | 3    | A `must_succeed` `post_apply` hook failed; file operations rolled back. |
//! | 4    | Exclusive-lock acquisition timed out (`apply` / `rollback`).  |
//! | 5    | Interactive prompt declined (the user entered anything other than `y`/`Y`). |
//!
//! The numeric values are the contract — downstream tooling and the
//! integration suite assert on them — so the discriminants are pinned
//! explicitly rather than left to declaration order.

use patina_core::EngineError;
use patina_core::LockError;

/// A terminal CLI outcome and its required process exit code (REQ-022).
///
/// The `#[repr(i32)]` and pinned discriminants make the numeric contract
/// part of the type: [`ExitCode::code`] returns the discriminant, and the
/// process terminates with exactly that integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// `0` — the command completed successfully.
    Success = 0,
    /// `1` — a generic failure (config parse, IO, undefined variable,
    /// journal version mismatch, missing prior apply, unresolved shell).
    Generic = 1,
    /// `2` — a `must_succeed` `pre_apply` hook failed; the apply aborted
    /// before performing any file operation.
    PreApplyAbort = 2,
    /// `3` — a `must_succeed` `post_apply` hook failed; the file
    /// operations were rolled back.
    PostApplyRollback = 3,
    /// `4` — the exclusive advisory lock could not be acquired within the
    /// configured timeout (`apply` / `rollback`).
    LockTimeout = 4,
    /// `5` — the user declined the interactive confirmation prompt (or, once
    /// SPEC-0002 lands, refused an elevation request).
    UserDeclined = 5,
}

impl ExitCode {
    /// The numeric process exit code this outcome maps to.
    #[must_use = "the returned integer is the process's terminal status"]
    pub fn code(self) -> i32 {
        self as i32
    }

    /// Map an [`EngineError`] to the exit code REQ-022 assigns it.
    ///
    /// Only the exclusive-lock timeout earns a dedicated code (`4`); every
    /// other engine failure is a generic error (`1`). The hook-driven codes
    /// (`2`, `3`) and the declined-prompt code (`5`) never travel as an
    /// `EngineError` — the engine reports a failed `must_succeed` hook as an
    /// `ApplyResult` outcome, and a declined prompt is a control-flow
    /// decision in the command layer — so they are not produced here.
    #[must_use = "the returned exit code is the process's terminal status"]
    pub fn from_engine_error(error: &EngineError) -> Self {
        match error {
            EngineError::Lock(LockError::Timeout { .. }) => ExitCode::LockTimeout,
            _ => ExitCode::Generic,
        }
    }

    /// Map the error chain of an `anyhow::Error` to an exit code.
    ///
    /// The command layer wraps engine failures with `anyhow` context, so the
    /// `EngineError` is rarely the outermost error. This walks the chain for
    /// the first [`EngineError`] and applies [`ExitCode::from_engine_error`];
    /// a chain carrying no `EngineError` (a pure presentation-layer failure)
    /// falls through to [`ExitCode::Generic`].
    #[must_use = "the returned exit code is the process's terminal status"]
    pub fn from_error_chain(error: &anyhow::Error) -> Self {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<EngineError>())
            .map_or(ExitCode::Generic, ExitCode::from_engine_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use patina_core::LockKind;
    use std::time::Duration;

    fn lock_timeout() -> EngineError {
        EngineError::Lock(LockError::Timeout {
            kind: LockKind::Exclusive,
            path: Utf8PathBuf::from("/state/lock"),
            waited: Duration::from_mins(1),
        })
    }

    #[test]
    fn lock_timeout_maps_to_four() {
        assert_eq!(
            ExitCode::from_engine_error(&lock_timeout()),
            ExitCode::LockTimeout
        );
    }

    #[test]
    fn non_timeout_lock_error_maps_to_generic() {
        let io = EngineError::Lock(LockError::Io {
            path: Utf8PathBuf::from("/state/lock"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        });
        assert_eq!(ExitCode::from_engine_error(&io), ExitCode::Generic);
    }

    #[test]
    fn other_engine_errors_map_to_generic() {
        // Any non-lock-timeout EngineError falls through to the generic
        // bucket; a state-directory failure stands in for "some other
        // subsystem error".
        let err = EngineError::StateDir(patina_core::StateDirError::MissingEnv { name: "HOME" });
        assert_eq!(ExitCode::from_engine_error(&err), ExitCode::Generic);
    }

    #[test]
    fn error_chain_finds_wrapped_lock_timeout() {
        let wrapped = anyhow::Error::new(lock_timeout()).context("apply execution failed");
        assert_eq!(ExitCode::from_error_chain(&wrapped), ExitCode::LockTimeout);
    }

    #[test]
    fn error_chain_without_engine_error_is_generic() {
        let bare = anyhow::anyhow!("a pure presentation failure");
        assert_eq!(ExitCode::from_error_chain(&bare), ExitCode::Generic);
    }
}
