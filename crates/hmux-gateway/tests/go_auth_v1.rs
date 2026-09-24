use chrono::{DateTime, Utc};
use hmux_gateway::auth::{
    account_profile, csrf_token, parse_credentials, parse_session_file, token_hash,
    usage_preference_key, validate_session_file, Credentials, PersistedSession,
    PersistedSessionFile,
};
use serde_json::Value;
use sha2::Digest;
use std::collections::HashMap;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/auth-v1/synthetic.json"
    ))
    .unwrap()
}

fn credentials(case: &Value) -> Credentials {
    parse_credentials(case["credential_json"].as_str().unwrap().as_bytes()).unwrap()
}

#[test]
fn go_credentials_crypto_and_json() {
    let f = fixture();
    let enabled = credentials(&f["enabled"]);
    let disabled = credentials(&f["disabled"]);
    for (credentials, case) in [(&enabled, &f["enabled"]), (&disabled, &f["disabled"])] {
        assert_eq!(
            credentials.go_json(),
            case["credential_json"].as_str().unwrap()
        );
        assert_eq!(
            credentials.fingerprint_input(),
            case["fingerprint_input"].as_str().unwrap()
        );
        assert_eq!(
            credentials.fingerprint(),
            case["fingerprint"].as_str().unwrap()
        );
        assert!(credentials.matches_password(f["password"].as_str().unwrap()));
        assert!(!credentials.matches_password("wrong-password"));
    }
    assert_ne!(enabled.fingerprint(), disabled.fingerprint());
    let mut replay_advanced = enabled.clone();
    replay_advanced.last_step += 1;
    assert_eq!(enabled.fingerprint(), replay_advanced.fingerprint());
    let seconds = f["unix_seconds"].as_i64().unwrap();
    let step = f["step"].as_i64().unwrap();
    assert_eq!(
        enabled.match_code(f["totp_code"].as_str().unwrap(), seconds),
        Some(step)
    );
    assert_eq!(
        enabled.match_code(f["totp_lower_code"].as_str().unwrap(), seconds),
        Some(step - 1)
    );
    let mut consumed = enabled.clone();
    consumed.last_step = step;
    assert_eq!(
        consumed.matches_unused_code(f["totp_code"].as_str().unwrap(), seconds),
        None
    );
    assert_eq!(enabled.match_code("abcdef", seconds), None);
    assert_eq!(enabled.match_code("1234567", seconds), None);
}

#[test]
fn go_tokens_and_sessions() {
    let f = fixture();
    let token = f["token"].as_str().unwrap();
    assert_eq!(token_hash(token), f["token_hash"].as_str().unwrap());
    assert_eq!(csrf_token(token), f["csrf"].as_str().unwrap());
    let file = parse_session_file(f["session_json"].as_str().unwrap().as_bytes()).unwrap();
    assert_eq!(file.go_json(), f["session_json"].as_str().unwrap());
    let c = credentials(&f["enabled"]);
    let mut accounts = HashMap::new();
    accounts.insert(c.username.clone(), (String::new(), c.clone()));
    let now: DateTime<Utc> = file.sessions.as_ref().unwrap()[0]
        .last_seen_at
        .parse()
        .unwrap();
    let validated = validate_session_file(&file, now, &accounts).unwrap();
    assert_eq!(validated.retained.len(), 1);
    assert!(!validated.dirty);
    let mut changed = c;
    changed.totp_disabled = true;
    accounts.insert(changed.username.clone(), (String::new(), changed));
    let validated = validate_session_file(&file, now, &accounts).unwrap();
    assert!(validated.retained.is_empty() && validated.dirty);
}

#[test]
fn strict_and_bounded_decoding() {
    let f = fixture();
    let mut credential = f["enabled"]["credentials"].clone();
    credential["unexpected"] = Value::Bool(true);
    assert!(parse_credentials(serde_json::to_string(&credential).unwrap().as_bytes()).is_err());
    let raw = f["enabled"]["credential_json"].as_str().unwrap();
    assert!(parse_credentials(format!("{raw} {{}}").as_bytes()).is_err());
    credential.as_object_mut().unwrap().remove("unexpected");
    credential["salt"] = Value::String("AA==".into());
    assert!(parse_credentials(serde_json::to_string(&credential).unwrap().as_bytes()).is_err());
    let mut sessions = parse_session_file(f["session_json"].as_str().unwrap().as_bytes()).unwrap();
    sessions.sessions.as_mut().unwrap()[0].browser = "bad\nlabel".into();
    let c = credentials(&f["enabled"]);
    let mut accounts = HashMap::new();
    accounts.insert(c.username.clone(), (String::new(), c));
    let now: DateTime<Utc> = "2024-02-03T04:06:06Z".parse().unwrap();
    assert!(validate_session_file(&sessions, now, &accounts).is_err());
    assert!(parse_session_file(br#"{"version":1,"sessions":[],"unknown":true}"#).is_err());
    assert!(parse_credentials(br#"["user","AA==","AA==","AAAA",0]"#).is_err());
    assert!(parse_session_file(br#"[1,[]]"#).is_err());
    let nil = parse_session_file(br#"{"version":1,"sessions":null}"#).unwrap();
    assert_eq!(nil.go_json(), r#"{"version":1,"sessions":null}"#);
    let empty = parse_session_file(br#"{"version":1,"sessions":[]}"#).unwrap();
    assert_eq!(empty.go_json(), r#"{"version":1,"sessions":[]}"#);
    assert_eq!(
        account_profile("synthetic"),
        format!("{:x}", sha2::Sha256::digest(b"synthetic"))
    );
    assert_ne!(
        usage_preference_key("a", "b"),
        usage_preference_key("a", "c")
    );
}

#[test]
fn go_secret_lengths_and_redacted_debug() {
    let f = fixture();
    let c = credentials(&f["enabled"]);
    for case in f["secret_cases"].as_array().unwrap() {
        let mut candidate = c.clone();
        candidate.totp_secret = case["secret"].as_str().unwrap().into();
        assert_eq!(
            candidate.validate().is_ok(),
            case["go_load_valid"].as_bool().unwrap(),
            "{}",
            case["label"]
        );
    }
    let debug = format!("{c:?}");
    assert!(!debug.contains(&c.username));
    assert!(!debug.contains(&c.totp_secret));
    assert!(!debug.contains(f["enabled"]["credentials"]["hash"].as_str().unwrap()));
    let file = parse_session_file(f["session_json"].as_str().unwrap().as_bytes()).unwrap();
    let debug = format!("{file:?} {:?}", file.sessions.as_ref().unwrap()[0]);
    assert!(!debug.contains(f["token_hash"].as_str().unwrap()));
    assert!(!debug.contains(f["token"].as_str().unwrap()));
    assert!(!debug.contains(&c.username));
}

#[test]
fn session_time_and_identity_edges() {
    let f = fixture();
    let file = parse_session_file(f["session_json"].as_str().unwrap().as_bytes()).unwrap();
    let c = credentials(&f["enabled"]);
    let mut accounts = HashMap::new();
    accounts.insert(c.username.clone(), (String::new(), c));
    let now: DateTime<Utc> = file.sessions.as_ref().unwrap()[0]
        .last_seen_at
        .parse()
        .unwrap();
    let mut bad = file.clone();
    bad.sessions.as_mut().unwrap()[0].created_at = "0001-01-01T00:00:00Z".into();
    assert!(validate_session_file(&bad, now, &accounts).is_err());
    let mut bad = file.clone();
    bad.sessions.as_mut().unwrap()[0].expires_at = "2024-02-10T04:05:07.123456789Z".into();
    assert!(validate_session_file(&bad, now, &accounts).is_err());
    let mut bad = file.clone();
    bad.sessions.as_mut().unwrap()[0].last_seen_at = "2024-02-03T04:05:05Z".into();
    assert!(validate_session_file(&bad, now, &accounts).is_err());
    let mut bad = file.clone();
    let duplicate = bad.sessions.as_ref().unwrap()[0].clone();
    bad.sessions.as_mut().unwrap().push(duplicate);
    assert!(validate_session_file(&bad, now, &accounts).is_err());
    let expiry: DateTime<Utc> = file.sessions.as_ref().unwrap()[0]
        .expires_at
        .parse()
        .unwrap();
    let expired = validate_session_file(&file, expiry, &accounts).unwrap();
    assert!(expired.retained.is_empty() && expired.dirty);
}

#[test]
fn public_auth_dtos_reject_positional_rows() {
    let f = fixture();
    let valid = f["session_json"].as_str().unwrap();
    let file: PersistedSessionFile = serde_json::from_str(valid).unwrap();
    assert_eq!(file.sessions.as_ref().unwrap().len(), 1);
    let row = f["session_file"]["sessions"][0].clone();
    assert!(serde_json::from_value::<PersistedSession>(row.clone()).is_ok());
    let keys = [
        "token_hash",
        "id",
        "username",
        "profile",
        "credential_fingerprint",
        "browser",
        "ip",
        "created_at",
        "last_seen_at",
        "expires_at",
    ];
    let positional: Vec<Value> = keys.iter().map(|key| row[*key].clone()).collect();
    assert!(serde_json::from_value::<PersistedSession>(Value::Array(positional.clone())).is_err());
    let mut file_value = f["session_file"].clone();
    file_value["sessions"] = Value::Array(vec![Value::Array(positional)]);
    assert!(serde_json::from_value::<PersistedSessionFile>(file_value.clone()).is_err());
    assert!(parse_session_file(serde_json::to_string(&file_value).unwrap().as_bytes()).is_err());
    assert!(serde_json::from_str::<PersistedSessionFile>("[1,[]]").is_err());
    assert!(serde_json::from_str::<Credentials>("[]").is_err());
    assert!(serde_json::from_value::<Credentials>(f["enabled"]["credentials"].clone()).is_ok());
    // Go's Unmarshal(null, &struct) leaves a zero value; file loading still validates it.
    assert_eq!(
        serde_json::from_str::<PersistedSession>("null").unwrap(),
        PersistedSession::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedSessionFile>("null").unwrap(),
        PersistedSessionFile::default()
    );
}
