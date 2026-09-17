//! Read-only service registry for Abora Framework.
//!
//! Cloud and Atlas must never re-implement service discovery. This crate is
//! the single place that knows how to list local init-manager services
//! (systemd on Linux) and map them onto the versioned API types defined in
//! [`abora_api`].
//!
//! # Separation of concerns
//!
//! * The [`ServiceCollector`] trait and the [`collect`] entry point are
//!   platform-agnostic.
//! * All Linux specifics live in the private [`linux`] module behind
//!   `cfg(target_os = "linux")`. The systemd backend runs `systemctl` with a
//!   **fixed** argument list — there is no user-controlled input, so there
//!   is no command-injection surface.
//!
//! Everything is read-only: this crate never starts, stops, or modifies a
//! service.

#![forbid(unsafe_code)]

pub use abora_api::{ServiceState, ServiceStatus};

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