//! Read-only service registry for Abora Framework.
//!
//! Cloud and Atlas must never re-implement service discovery. This crate is
//! the single place that knows how to list local init-manager services
//! (systemd on Linux) and map them onto the versioned API types defined in
//! [`abora_api`].
//!
//! # Separation of concerns
//!
//! * The [`ServiceCollector`] trait and the [`collect`]/[`detail`] entry
//!   points are platform-agnostic.
//! * All Linux specifics live in the private [`linux`] module behind
//!   `cfg(target_os = "linux")`. The systemd backend runs `systemctl` with a
//!   **fixed** argument list — there is no user-controlled input, so there
//!   is no command-injection surface. Free-form unit names are validated by
//!   [`validate_unit_name`] so they can never be smuggled into a command
//!   line.
//!
//! Everything is read-only: this crate never starts, stops, or modifies a
//! service.
//!
//! # Filtering and pagination
//!
//! [`ServiceQuery`] is a pure, platform-agnostic filter + pager. [`collect`]
//! returns the full, sorted list and [`ServiceQuery::apply`] narrows it:
//! filtering and pagination stay in one place and are unit-testable without
//! a socket.

#![forbid(unsafe_code)]

pub use abora_api::{ServiceDetail, ServiceState, ServiceStatus};

#[cfg(target_os = "linux")]
mod linux;

/// Errors from service discovery.
#[derive(Debug, thiserror::Error)]
pub enum ServicesError {
    /// Running on a platform without a backend.
    #[error("service registry is not supported on this platform")]
    NotSupported,
    /// The local service manager could not be queried.
    #[error("could not query the system service manager: {0}")]
    Systemd(String),
    /// The requested unit does not exist.
    #[error("no such unit: {0}")]
    NotFound(String),
    /// The service manager produced output we did not expect.
    #[error("unexpected output from the system service manager: {0}")]
    Parse(String),
}

/// Abstraction over the local service manager.
pub trait ServiceCollector {
    /// List every managed service and its current state. Read-only.
    fn collect(&self) -> Result<Vec<ServiceStatus>, ServicesError>;
}

/// Discover services on the local machine.
pub fn collect() -> Result<Vec<ServiceStatus>, ServicesError> {
    #[cfg(target_os = "linux")]
    {
        linux::SystemdServiceManager.collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(ServicesError::NotSupported)
    }
}

/// Fetch the detailed status of one unit by name.
pub fn detail(name: &str) -> Result<ServiceDetail, ServicesError> {
    let name = name.trim();
    if !validate_unit_name(name) {
        return Err(ServicesError::Systemd(format!(
            "`{name}` is not a valid unit name"
        )));
    }
    #[cfg(target_os = "linux")]
    {
        linux::SystemdServiceManager.detail(name)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        Err(ServicesError::NotSupported)
    }
}

/// Unit names are passed as arguments to `systemctl` inside this crate, so
/// they are checked defensively even though the argument list is fixed:
/// unit names must be relative (no `/` or `\`), must not start with `-`
/// (option injection), and conventionally end in `.service`.
pub fn validate_unit_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains(['/', '\\', ' ', '\t', '\n', '\r'])
        && name.ends_with(".service")
}

/// How one unit's `ActiveState`/name/description should be compared. The
/// `state` filter matches the coarse [`ServiceState`]; `enabled` matches the
/// `enabled` hint (`true`/`false`); `text` is a case-insensitive substring
/// over name and description.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceQuery {
    pub state: Option<ServiceState>,
    pub enabled: Option<bool>,
    pub text: Option<String>,
    pub limit: Option<usize>,
    pub offset: usize,
}

impl ServiceQuery {
    /// How many units match the filters, ignoring pagination.
    pub fn total_matching(&self, services: &[ServiceStatus]) -> usize {
        services.iter().filter(|s| self.matches(s)).count()
    }

    /// Apply filters then paginate, returning the matching units (clones;
    /// `ServiceStatus` is just a few `String`s).
    pub fn apply(&self, services: &[ServiceStatus]) -> Vec<ServiceStatus> {
        let matched: Vec<&ServiceStatus> = services.iter().filter(|s| self.matches(s)).collect();
        if matched.is_empty() {
            return Vec::new();
        }
        let start = self.offset.min(matched.len());
        let end = match self.limit {
            Some(limit) if limit > 0 => (start + limit).min(matched.len()),
            _ => matched.len(),
        };
        matched[start..end].iter().map(|s| (*s).clone()).collect()
    }

    /// Whether a service satisfies the filters (no pagination).
    pub fn matches(&self, service: &ServiceStatus) -> bool {
        if let Some(state) = self.state {
            if service.state != state {
                return false;
            }
        }
        if let Some(enabled) = self.enabled {
            if service.enabled != Some(enabled) {
                return false;
            }
        }
        if let Some(text) = &self.text {
            let needle = text.to_ascii_lowercase();
            let name_hit = service.name.to_ascii_lowercase().contains(&needle);
            let desc_hit = service
                .description
                .as_deref()
                .map(|d| d.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false);
            if !name_hit && !desc_hit {
                return false;
            }
        }
        true
    }
}
