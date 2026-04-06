use crate::domains::ops::HealthState;

#[must_use]
pub fn summarize_health_state(states: &[HealthState]) -> HealthState {
    if states.iter().any(|state| matches!(state, HealthState::Blocked)) {
        HealthState::Blocked
    } else if states.iter().any(|state| matches!(state, HealthState::Misconfigured)) {
        HealthState::Misconfigured
    } else if states.iter().any(|state| matches!(state, HealthState::Unavailable)) {
        HealthState::Unavailable
    } else if states.iter().any(|state| matches!(state, HealthState::Degraded)) {
        HealthState::Degraded
    } else {
        HealthState::Healthy
    }
}
