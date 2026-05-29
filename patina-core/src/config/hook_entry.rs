//! `[[hook]]` table-array schema (REQ-006).
//!
//! Each `[[hook]]` entry resolves to a [`HookEntry`] carrying an
//! [`HookEvent`], a shell command, an optional explicit shell, an
//! optional raw `when` expression, and a `must_succeed` boolean that
//! defaults to `true` when omitted. Parse-time validation rejects the
//! `on_change` / `on_drift` legacy event names with a typed error that
//! names the offending value and the two accepted event names so the
//! CHK-013 substring contract holds.

use serde::Deserialize;

/// Supported hook event kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Hook fires before the apply plan executes any file operations.
    PreApply,
    /// Hook fires after every file operation in the plan has executed.
    PostApply,
}

/// A validated `[[hook]]` table-array entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    /// When the hook fires.
    pub event: HookEvent,
    /// Shell command to execute.
    pub command: String,
    /// Optional explicit shell (e.g. `"bash"`, `"pwsh"`). Stored
    /// verbatim — the shell-on-PATH check is deferred to T-015.
    pub shell: Option<String>,
    /// Optional `when` predicate as raw expression source. Parsing
    /// / evaluation through `MiniJinja` is deferred to T-008.
    pub when: Option<String>,
    /// If `true`, a non-zero exit from the hook aborts the apply.
    /// Defaults to `true` when the field is omitted (REQ-006 rule 2).
    pub must_succeed: bool,
}

/// Parse-time failures from REQ-006's `[[hook]]` table-array rules.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HookEntryError {
    /// `event` was set to a value outside the accepted pair. The two
    /// accepted values are listed in the message so the CHK-013
    /// substring contract holds.
    #[error(
        "[[hook]] entry declares unsupported event `{value}`; the accepted values are `pre_apply` and `post_apply`"
    )]
    UnsupportedEvent {
        /// The offending event string.
        value: String,
    },
}

impl HookEntry {
    /// Build a [`HookEntry`] from a raw deserialized [`RawHookEntry`],
    /// applying REQ-006's parse-time rules.
    pub(super) fn from_raw(raw: RawHookEntry) -> Result<Self, HookEntryError> {
        let RawHookEntry {
            event,
            command,
            shell,
            when,
            must_succeed,
        } = raw;

        let resolved_event = match event.as_str() {
            "pre_apply" => HookEvent::PreApply,
            "post_apply" => HookEvent::PostApply,
            _ => {
                return Err(HookEntryError::UnsupportedEvent { value: event });
            }
        };

        Ok(Self {
            event: resolved_event,
            command,
            shell,
            when,
            must_succeed: must_succeed.unwrap_or(true),
        })
    }
}

/// Raw TOML projection of a `[[hook]]` entry.
#[derive(Debug, Deserialize)]
pub(super) struct RawHookEntry {
    pub(super) event: String,
    pub(super) command: String,
    #[serde(default)]
    pub(super) shell: Option<String>,
    #[serde(default)]
    pub(super) when: Option<String>,
    #[serde(default)]
    pub(super) must_succeed: Option<bool>,
}
