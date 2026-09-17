//! Authorization model for `aborad`.
//!
//! # Current milestone guarantees
//!
//! * The daemon only ever binds a loopback address; remote management is
//!   deliberately not implemented yet (see `docs/security.md`).
//! * Every route declares the [`Permission`] it requires
//!   (`src/server.rs`), so per-operation authorization checks are
//!   structurally present even though today's only admission rule is
//!   "loopback clients are trusted".
//! * There is **no arbitrary command execution endpoint** anywhere.
//!
//! # Future
//!
//! Replace the loopback trust with token/mTLS authentication, then map each
//! [`Permission`] to an actual authorization policy. The pure function
//! [`decision`] is the seam where that logic lands.

use std::net::IpAddr;

use abora_api::ApiErrorBody;
use abora_config::Config;

/// A single privileged capability a handler requires. Every route must
/// declare one; none may run without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ReadHealth,
    ReadVersion,
    ReadSystem,
    ReadServices,
    ReadUpdates,
}

impl Permission {
    /// Machine-readable identifier for audit logs and future policy files.
    pub const fn permission_id(self) -> &'static str {
        match self {
            Permission::ReadHealth => "read:health",
            Permission::ReadVersion => "read:version",
            Permission::ReadSystem => "read:system",
            Permission::ReadServices => "read:services",
            Permission::ReadUpdates => "read:updates",
        }
    }

    /// Only read-only permissions exist in this milestone. Mutating
    /// permissions (and the audit requirements that come with them) will be
    /// added alongside real authenticated remote management.
    #[allow(dead_code)]
    pub const fn is_mutating(self) -> bool {
        false
    }
}

/// Why a request was (or was not) admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Client is not trusted on this transport (only loopback is trusted).
    RemoteNotAllowed,
    /// Loopback client but authentication is required and there is no
    /// credential mechanism available yet (misconfiguration for now).
    AuthenticationRequired,
}

/// Pure admission decision. Security-critical logic lives here so it can be
/// unit tested without a socket.
pub fn decision(peer: IpAddr, config: &Config) -> Decision {
    if !peer.is_loopback() {
        return Decision::RemoteNotAllowed;
    }

    if config.security.require_authentication
        && !config.security.allow_loopback_unauthenticated
    {
        // No credential verification exists yet. Rather than silently
        // treating everyone as authenticated, we refuse loudly.
        return Decision::AuthenticationRequired;
    }

    Decision::Allowed
}

impl Decision {
    /// Convert into an API error body, if the decision was a denial.
    pub fn into_denial(self, permission: Permission) -> Option<ApiErrorBody> {
        match self {
            Decision::Allowed => None,
            Decision::RemoteNotAllowed => Some(ApiErrorBody::new(
                abora_api::ErrorCode::Forbidden,
                format!(
                    "operation `{}` is not allowed for remote clients; remote management is not enabled",
                    permission.permission_id()
                ),
            )),
            Decision::AuthenticationRequired => Some(ApiErrorBody::new(
                abora_api::ErrorCode::Unauthorized,
                format!(
                    "operation `{}` requires authentication, which is not available in this milestone; set [security].allow_loopback_unauthenticated = true to use loopback-only mode",
                    permission.permission_id()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn loopback_is_allowed_by_default() {
        assert_eq!(
            decision(IpAddr::V4(Ipv4Addr::LOCALHOST), &cfg()),
            Decision::Allowed
        );
        assert_eq!(
            decision(IpAddr::V6(Ipv6Addr::LOCALHOST), &cfg()),
            Decision::Allowed
        );
    }

    #[test]
    fn remote_clients_are_refused() {
        let peers = [
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            IpAddr::V6(Ipv6Addr::from(0x2001_0db8_0000_0000_0000_0000_0000_0001_u128)),
        ];
        for peer in peers {
            assert_eq!(decision(peer, &cfg()), Decision::RemoteNotAllowed);
        }
    }

    #[test]
    fn loopback_with_hard_auth_requires_credential_mechanism() {
        let mut c = cfg();
        c.security.allow_loopback_unauthenticated = false;
        assert_eq!(
            decision(IpAddr::V4(Ipv4Addr::LOCALHOST), &c),
            Decision::AuthenticationRequired
        );
    }

    #[test]
    fn permission_ids_are_stable_identifiers() {
        assert_eq!(Permission::ReadSystem.permission_id(), "read:system");
        assert!(!Permission::ReadHealth.is_mutating());
    }

    #[test]
    fn denial_maps_to_expected_error_codes() {
        let denial = Decision::RemoteNotAllowed
            .into_denial(Permission::ReadSystem)
            .unwrap();
        assert_eq!(denial.error.code, abora_api::ErrorCode::Forbidden);

        let denial = Decision::AuthenticationRequired
            .into_denial(Permission::ReadHealth)
            .unwrap();
        assert_eq!(denial.error.code, abora_api::ErrorCode::Unauthorized);

        assert!(Decision::Allowed.into_denial(Permission::ReadHealth).is_none());
    }
}