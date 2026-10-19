use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Browser session lifetime policy.
///
/// A session lives under two independent deadlines:
///
/// * a **sliding idle window** — the session dies once it has gone unused for
///   `idle_ttl_hours`, and every authenticated request pushes that window
///   forward by refreshing `last_seen_at`;
/// * an **absolute cap** — the session dies `absolute_ttl_hours` after sign-in
///   no matter how active it was. The cap is stamped onto the session row as
///   `expires_at` when it is created, so it is a fixed point in time and one
///   stored value rather than a rule re-derived on every request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLifetimePolicy {
    pub idle_ttl_hours: u64,
    pub absolute_ttl_hours: u64,
}

/// The stored facts a session lifetime decision is made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLifetimeSnapshot {
    /// Absolute deadline stamped at sign-in.
    pub expires_at: DateTime<Utc>,
    /// Last authenticated request seen on this session.
    pub last_seen_at: DateTime<Utc>,
    /// Set when the session was signed out or revoked by an operator.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Why a session may or may not authenticate a request.
///
/// The two expiry reasons are kept apart because they answer different
/// operational questions ("did they walk away" vs "did the cap run out") and
/// because they are the two cases the policy has to be tested against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifetimeState {
    Active,
    IdleExpired,
    AbsoluteExpired,
    Revoked,
}

impl SessionLifetimeState {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Convert a configured hour count into a duration, clamping a nonsensical
/// value to at least one hour so a misconfigured deployment cannot end up
/// expiring every session the instant it is issued.
fn ttl_duration(hours: u64) -> Duration {
    let hours = i64::try_from(hours.max(1)).unwrap_or(i64::MAX);
    Duration::try_hours(hours).unwrap_or(Duration::MAX)
}

impl SessionLifetimePolicy {
    /// Absolute deadline to stamp on a session issued at `issued_at`.
    #[must_use]
    pub fn absolute_expiry_at(self, issued_at: DateTime<Utc>) -> DateTime<Utc> {
        issued_at
            .checked_add_signed(ttl_duration(self.absolute_ttl_hours))
            .unwrap_or(DateTime::<Utc>::MAX_UTC)
    }

    /// Decide whether a stored session may still authenticate a request at
    /// `now`. Pure: no clock, no database, no configuration lookup.
    #[must_use]
    pub fn evaluate(
        self,
        snapshot: SessionLifetimeSnapshot,
        now: DateTime<Utc>,
    ) -> SessionLifetimeState {
        if snapshot.revoked_at.is_some() {
            return SessionLifetimeState::Revoked;
        }
        if now >= snapshot.expires_at {
            return SessionLifetimeState::AbsoluteExpired;
        }
        let idle_deadline = snapshot
            .last_seen_at
            .checked_add_signed(ttl_duration(self.idle_ttl_hours))
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        if now >= idle_deadline {
            return SessionLifetimeState::IdleExpired;
        }
        SessionLifetimeState::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub enum PrincipalKind {
    User,
    ApiToken,
    Worker,
    Bootstrap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub enum GrantResourceKind {
    System,
    Workspace,
    Library,
    Document,
    QuerySession,
    AsyncOperation,
    Connector,
    ProviderCredential,
    LibraryBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceMembership {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Principal {
    pub id: Uuid,
    pub principal_kind: PrincipalKind,
    pub display_label: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiToken {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Grant {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: GrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped defaults: sign-in survives three days of silence, and no
    /// session outlives thirty days regardless of how busy it was.
    const POLICY: SessionLifetimePolicy =
        SessionLifetimePolicy { idle_ttl_hours: 72, absolute_ttl_hours: 720 };

    fn at(hours_from_epoch: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + Duration::hours(hours_from_epoch)
    }

    fn snapshot(expires_at_hours: i64, last_seen_at_hours: i64) -> SessionLifetimeSnapshot {
        SessionLifetimeSnapshot {
            expires_at: at(expires_at_hours),
            last_seen_at: at(last_seen_at_hours),
            revoked_at: None,
        }
    }

    #[test]
    fn absolute_deadline_is_the_configured_span_after_sign_in() {
        assert_eq!(POLICY.absolute_expiry_at(at(0)), at(720));
    }

    #[test]
    fn a_recently_used_session_inside_the_cap_stays_active() {
        // Signed in at hour 0, cap at 720, last request one hour ago.
        assert_eq!(POLICY.evaluate(snapshot(720, 500), at(501)), SessionLifetimeState::Active);
    }

    #[test]
    fn idle_window_expires_after_the_configured_silence() {
        // Well inside the 720h cap, but untouched for longer than 72h.
        assert_eq!(
            POLICY.evaluate(snapshot(720, 100), at(100 + 72)),
            SessionLifetimeState::IdleExpired,
        );
        // One hour before the idle deadline it is still usable, which is what
        // makes the boundary above meaningful rather than always-true.
        assert_eq!(POLICY.evaluate(snapshot(720, 100), at(100 + 71)), SessionLifetimeState::Active,);
    }

    #[test]
    fn activity_slides_the_idle_window_forward() {
        // Same session, same instant; only last_seen_at differs. A request at
        // hour 160 keeps a session alive that hour-100 activity would not.
        let now = at(180);
        assert_eq!(POLICY.evaluate(snapshot(720, 100), now), SessionLifetimeState::IdleExpired);
        assert_eq!(POLICY.evaluate(snapshot(720, 160), now), SessionLifetimeState::Active);
    }

    #[test]
    fn absolute_cap_expires_a_continuously_active_session() {
        // Used seconds ago — the idle window is nowhere near done — but the
        // 720h cap from sign-in has passed.
        assert_eq!(
            POLICY.evaluate(snapshot(720, 720), at(720)),
            SessionLifetimeState::AbsoluteExpired
        );
    }

    #[test]
    fn the_absolute_cap_outranks_a_fresh_idle_window() {
        // Both deadlines are in the past; the answer must name the cap, not
        // the idle window, so operators can tell the two cases apart.
        assert_eq!(
            POLICY.evaluate(snapshot(720, 500), at(900)),
            SessionLifetimeState::AbsoluteExpired
        );
    }

    #[test]
    fn revocation_wins_over_a_perfectly_healthy_session() {
        let snapshot = SessionLifetimeSnapshot {
            expires_at: at(720),
            last_seen_at: at(500),
            revoked_at: Some(at(501)),
        };
        assert_eq!(POLICY.evaluate(snapshot, at(502)), SessionLifetimeState::Revoked);
    }

    #[test]
    fn a_zero_hour_configuration_is_clamped_instead_of_expiring_everything() {
        let policy = SessionLifetimePolicy { idle_ttl_hours: 0, absolute_ttl_hours: 0 };
        assert_eq!(policy.absolute_expiry_at(at(0)), at(1));
        assert_eq!(
            policy.evaluate(snapshot(1, 0), at(0)),
            SessionLifetimeState::Active,
            "a session must not be dead the instant it is issued",
        );
    }

    #[test]
    fn only_the_active_state_authenticates() {
        assert!(SessionLifetimeState::Active.is_active());
        assert!(!SessionLifetimeState::IdleExpired.is_active());
        assert!(!SessionLifetimeState::AbsoluteExpired.is_active());
        assert!(!SessionLifetimeState::Revoked.is_active());
    }
}
