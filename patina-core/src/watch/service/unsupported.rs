//! The fallback service backend for hosts with no implemented supervisor
//! integration yet.
//!
//! The [`super::current`] factory returns this stub on any host whose concrete
//! backend has not landed. Its lifecycle methods all return
//! [`ServiceError::Unsupported`], whose message directs the user to the
//! foreground escape hatch (`patina watch --foreground`) under their own
//! supervisor. `status` reports a not-installed, not-running service rather
//! than erroring, so `patina watch status` still produces a clean object on an
//! unsupported host.

use super::LifecycleResult;
use super::ServiceBackend;
use super::ServiceError;
use super::ServiceStatus;

/// The no-supervisor fallback backend.
#[derive(Debug, Clone, Copy)]
pub struct UnsupportedBackend;

impl ServiceBackend for UnsupportedBackend {
    fn install(&self) -> Result<LifecycleResult, ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn uninstall(&self) -> Result<LifecycleResult, ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn start(&self) -> Result<LifecycleResult, ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn stop(&self) -> Result<LifecycleResult, ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn restart(&self) -> Result<LifecycleResult, ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn status(&self) -> Result<ServiceStatus, ServiceError> {
        // No supervisor integration: report a clean not-installed object so
        // `patina watch status` still emits a well-formed result.
        Ok(ServiceStatus {
            installed: false,
            running: false,
            last_fired_at: None,
            last_exit_code: None,
            subscriptions_count: None,
            re_applies_since_start: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_methods_return_the_unsupported_error() {
        let backend = UnsupportedBackend;
        assert!(matches!(backend.install(), Err(ServiceError::Unsupported)));
        assert!(matches!(
            backend.uninstall(),
            Err(ServiceError::Unsupported)
        ));
        assert!(matches!(backend.start(), Err(ServiceError::Unsupported)));
        assert!(matches!(backend.stop(), Err(ServiceError::Unsupported)));
        assert!(matches!(backend.restart(), Err(ServiceError::Unsupported)));
    }

    #[test]
    fn status_reports_a_clean_not_installed_object() {
        let backend = UnsupportedBackend;
        let status = backend.status().expect("unsupported status is Ok");
        assert!(!status.installed);
        assert!(!status.running);
        assert_eq!(status.last_exit_code, None);
        assert_eq!(status.subscriptions_count, None);
    }

    #[test]
    fn unsupported_error_message_points_at_foreground() {
        // the message must direct the user to the foreground escape
        // hatch so the not-yet-supported path is actionable.
        let message = ServiceError::Unsupported.to_string();
        assert!(
            message.contains("--foreground"),
            "the unsupported error must name the foreground escape hatch, got: {message}"
        );
    }
}
