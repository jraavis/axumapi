//! Cardinality and expiry checks without timing sleeps.

use super::{EXPIRY_SCAN_BUDGET, RateLimit};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::PoisonError;
use std::time::{Duration, Instant};

fn key(index: u32) -> Option<IpAddr> {
    Some(IpAddr::V4(Ipv4Addr::from(index)))
}

#[test]
fn high_cardinality_never_evicts_depleted_clients_or_grows_state() {
    let limiter = RateLimit::new(1, 1.0).max_clients(17);
    let now = Instant::now();
    for index in 0..100_000 {
        assert_eq!(limiter.take(key(index), now).is_ok(), index < 17);
    }
    assert!(limiter.take(key(0), now).is_err());
    let guard = limiter.buckets.lock();
    let state = guard.unwrap_or_else(PoisonError::into_inner);
    assert_eq!(state.entries.len(), 17);
    assert_eq!(state.expiry.len(), 17);
}

#[test]
fn fully_replenished_expiry_has_a_fixed_scan_budget() {
    let limiter = RateLimit::new(1, 1.0).max_clients(32);
    let now = Instant::now();
    for index in 0..32 {
        assert!(limiter.take(key(index), now).is_ok());
    }
    assert!(limiter.take(key(33), now + Duration::from_secs(1)).is_ok());
    let guard = limiter.buckets.lock();
    let state = guard.unwrap_or_else(PoisonError::into_inner);
    assert_eq!(state.entries.len(), 32 - EXPIRY_SCAN_BUDGET + 1);
    assert_eq!(state.expiry.len(), state.entries.len());
}

#[test]
fn invalid_numeric_configuration_is_rejected() {
    for rate in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(RateLimit::try_new(1, rate).is_err());
    }
    assert!(RateLimit::try_new(0, 1.0).is_err());
    assert!(RateLimit::try_new(1, f64::MIN_POSITIVE).is_ok());
    assert!(!RateLimit::new(1, 1.0).max_clients(0).valid());
}

#[test]
fn configuring_capacity_does_not_mutate_existing_clones() {
    let original = RateLimit::new(1, 1.0);
    let now = Instant::now();
    assert!(original.take(key(1), now).is_ok());
    let configured = original.clone().max_clients(1);
    assert!(configured.take(key(2), now).is_ok());
    assert!(configured.take(key(3), now).is_err());
    assert!(original.take(key(1), now).is_err());
    assert!(original.take(key(3), now).is_ok());
}
