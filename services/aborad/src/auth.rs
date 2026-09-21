//! Authorization model for `aborad`.
//!
//! # Rules (in order)
//!
//! 1. **Never allow remote peers.** Remote management is not implemented;
//!    non-loopback clients are always refused regardless of credentials.
//! 2. **No token store configured** (`[security] token_file` unset):
//!    loopback clients are trusted (preview mode). This is the current
//!    out-of-the-box posture; the daemon logs a warning about it.
//! 3. **Token store configured**: every request must present
//!    `Authorization: Bearer <secret>`. Unknown/missing secrets are rejected
//!    with `401`; a valid secret without the route's permission is rejected
//!    with `403`.
//!
//! The pure function [`authorize`] is the single decision point, so all of
//! this logic is unit-testable without a socket.

use std::net::IpAddr;

use abora_api::ApiErrorBody;

use crate::tokens::TokenStore;

/// A single privileged capability a handler requires. Every route must
/// declare one; none may run without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ReadHealth,
    ReadVersion,
    ReadSystem,
    ReadServices,
    ReadUpdates,
    /// Apply updates. Mutating: needs a token that names this permission explicitly.
    ManageUpdates,
}

impl Permission {
    /// Machine-readable identifier used by token grants and audit logs.
    pub const fn permission_id(self) -> &'static str {
        match self {
            Permission::ReadHealth => "read:health",
            Permission::ReadVersion => "read:version",
            Permission::ReadSystem => "read:system",
            Permission::ReadServices => "read:services",
            Permission::ReadUpdates => "read:updates",
            Permission::ManageUpdates => "manage:updates",
        }
    }

    /// Mutating permissions change the system. They are never available in
    /// preview mode (no token store), are never covered by `read_all`, and
    /// every use is logged with the caller's identity.
    pub const fn is_mutating(self) -> bool {
        matches!(self, Permission::ManageUpdates)
    }
}

/// Who made a request, recorded by the authorization layer for the audit trail. For tokens this
/// is the token's `name` (or a placeholder if it has none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller(pub String);

/// Why a request was (or was not) admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Client is not on loopback; remote management is not enabled.
    RemoteNotAllowed,
    /// Token required but missing or invalid (`401`).
    AuthenticationRequired,
    /// Token authentic but does not grant the requested permission (`403`).
    InsufficientPermission,
    /// A mutating operation was attempted but no token store is configured, so there is nobody
    /// to authenticate (`403`). Preview-mode loopback trust never covers mutation.
    TokensRequired,
}

/// Pure admission decision. The server passes `bearer` as parsed from the
/// `Authorization` header and `store` only when authentication is enforced
/// (i.e. `require_authentication` is true and a token file was loaded).
pub fn authorize(
    peer: IpAddr,
    permission: Permission,
    bearer: Option<&str>,
    store: Option<&TokenStore>,
) -> Decision {
    if !peer.is_loopback() {
        return Decision::RemoteNotAllowed;
    }

    let Some(store) = store else {
        // No token store: preview mode, loopback is trusted for reads only.
        return if permission.is_mutating() {
            Decision::TokensRequired
        } else {
            Decision::Allowed
        };
    };

    let Some(bearer) = bearer else {
        return Decision::AuthenticationRequired;
    };

    match store.find(bearer) {
        None => Decision::AuthenticationRequired,
        Some(token) if token.grants(permission.permission_id()) => Decision::Allowed,
        Some(_) => Decision::InsufficientPermission,
    }
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
                    "operation `{}` requires a valid bearer token",
                    permission.permission_id()
                ),
            )),
            Decision::TokensRequired => Some(ApiErrorBody::new(
                abora_api::ErrorCode::Forbidden,
                format!(
                    "operation `{}` changes the system and requires token authentication; configure [security] token_file and use a token that grants it",
                    permission.permission_id()
                ),
            )),
            Decision::InsufficientPermission => Some(ApiErrorBody::new(
                abora_api::ErrorCode::Forbidden,
                format!(
                    "token is authenticated but not authorized for `{}`",
                    permission.permission_id()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::TokenStore;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const REMOTE: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));

    fn store(secret: &str, permissions: &[&str]) -> TokenStore {
        let perms = permissions
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let hash = crate::tokens::sha256_hex(secret);
        TokenStore::from_str(&format!(
            "[[tokens]]\nname = \"test\"\npermissions = [{perms}]\nsecret_hash = \"{hash}\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn loopback_is_allowed_without_a_store() {
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadHealth, None, None),
            Decision::Allowed
        );
        assert_eq!(
            authorize(
                IpAddr::V6(Ipv6Addr::LOCALHOST),
                Permission::ReadHealth,
                None,
                None
            ),
            Decision::Allowed
        );
    }

    #[test]
    fn remote_clients_are_refused_even_with_valid_tokens() {
        let store = store("secret", &["read_all"]);
        assert_eq!(
            authorize(REMOTE, Permission::ReadHealth, Some("secret"), Some(&store)),
            Decision::RemoteNotAllowed
        );
        assert_eq!(
            authorize(REMOTE, Permission::ReadSystem, None, None),
            Decision::RemoteNotAllowed
        );
    }

    #[test]
    fn missing_header_is_401_when_a_store_exists() {
        let store = store("secret", &["read_all"]);
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadHealth, None, Some(&store)),
            Decision::AuthenticationRequired
        );
    }

    #[test]
    fn unknown_token_is_401() {
        let store = store("right", &["read_all"]);
        assert_eq!(
            authorize(
                LOOPBACK,
                Permission::ReadHealth,
                Some("wrong"),
                Some(&store)
            ),
            Decision::AuthenticationRequired
        );
    }

    #[test]
    fn valid_token_without_permission_is_403() {
        let store = store("ops", &["read:health"]);
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadSystem, Some("ops"), Some(&store)),
            Decision::InsufficientPermission
        );
    }

    #[test]
    fn valid_token_with_permission_is_allowed() {
        let store = store("ops", &["read:health"]);
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadHealth, Some("ops"), Some(&store)),
            Decision::Allowed
        );
    }

    #[test]
    fn read_all_grant_covers_every_permission() {
        let store = store("admin", &["read_all"]);
        for p in [
            Permission::ReadHealth,
            Permission::ReadVersion,
            Permission::ReadSystem,
            Permission::ReadServices,
            Permission::ReadUpdates,
        ] {
            assert_eq!(
                authorize(LOOPBACK, p, Some("admin"), Some(&store)),
                Decision::Allowed
            );
        }
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

        let denial = Decision::InsufficientPermission
            .into_denial(Permission::ReadHealth)
            .unwrap();
        assert_eq!(denial.error.code, abora_api::ErrorCode::Forbidden);

        assert!(Decision::Allowed
            .into_denial(Permission::ReadHealth)
            .is_none());
    }

    #[test]
    fn read_all_never_grants_a_mutating_permission() {
        let s = store("secret", &["read_all"]);
        assert_eq!(
            authorize(
                LOOPBACK,
                Permission::ManageUpdates,
                Some("secret"),
                Some(&s)
            ),
            Decision::InsufficientPermission
        );
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadUpdates, Some("secret"), Some(&s)),
            Decision::Allowed
        );
    }

    #[test]
    fn a_mutating_permission_must_be_named_explicitly() {
        let s = store("secret", &["manage:updates"]);
        assert_eq!(
            authorize(
                LOOPBACK,
                Permission::ManageUpdates,
                Some("secret"),
                Some(&s)
            ),
            Decision::Allowed
        );
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadUpdates, Some("secret"), Some(&s)),
            Decision::InsufficientPermission,
            "manage does not imply read"
        );
    }

    #[test]
    fn preview_mode_never_allows_mutation() {
        assert_eq!(
            authorize(LOOPBACK, Permission::ManageUpdates, None, None),
            Decision::TokensRequired
        );
        assert_eq!(
            authorize(LOOPBACK, Permission::ReadUpdates, None, None),
            Decision::Allowed
        );
        assert!(!Permission::ReadUpdates.is_mutating() && Permission::ManageUpdates.is_mutating());
    }
}
