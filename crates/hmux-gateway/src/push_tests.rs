use super::*;

fn session(created_at: i64) -> SessionIdentity {
    SessionIdentity {
        id: "$1".into(),
        created_at,
    }
}

#[test]
fn presence_is_exact_bounded_and_expires() {
    let now = Instant::now();
    let mut state = Transient::default();
    for login in 0..256 {
        for client in 0..16 {
            state.update_presence(
                &format!("login-{login}"),
                &format!("client-{client}"),
                Some(session(17)),
                now,
            );
        }
    }
    assert_eq!(state.presence.len(), 256);
    assert!(state.watching("login-1", &session(17), now));
    assert!(!state.watching("login-1", &session(18), now));
    state.update_presence("login-1", "overflow", Some(session(17)), now);
    assert_eq!(state.presence["login-1"].len(), 16);
    state.update_presence("overflow", "client", Some(session(17)), now);
    assert!(!state.presence.contains_key("overflow"));
    state.update_presence("login-1", "client-0", None, now);
    assert_eq!(state.presence["login-1"].len(), 15);
    state.update_presence("login-1", "replacement", Some(session(18)), now);
    assert!(state.watching("login-1", &session(18), now));
    assert!(!state.watching("login-1", &session(17), now + PRESENCE_TTL));
    assert!(state.presence.is_empty());
}

#[test]
fn test_rate_is_per_login_and_never_exceeds_subscription_cap() {
    let now = Instant::now();
    let mut state = Transient::default();
    for login in 0..256 {
        assert!(state.test_admit(&format!("login-{login}"), now));
    }
    assert!(!state.test_admit("login-0", now));
    assert!(!state.test_admit("overflow", now));
    assert_eq!(state.last_test.len(), 256);
    assert!(state.test_admit("overflow", now + TEST_INTERVAL));
    assert_eq!(state.last_test.len(), 1);
    let allowed = HashSet::from(["different".to_owned()]);
    state.retain_subscribers(&allowed);
    assert!(state.last_test.is_empty());
}
