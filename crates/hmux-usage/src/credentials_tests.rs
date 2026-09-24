use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    provider: String,
    body: String,
}

#[derive(Deserialize)]
struct Oracle {
    name: String,
    token: String,
    account_id: String,
    identity: String,
    digest_hex: String,
}

#[test]
fn matches_actual_go_credential_oracle() {
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-credentials-v1/cases.json"
    ))
    .unwrap();
    let oracle: Vec<Oracle> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-credentials-v1/go-oracle.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), oracle.len());
    for (case, expected) in cases.iter().zip(&oracle) {
        assert_eq!(case.name, expected.name);
        let provider = match case.provider.as_str() {
            "claude" => Provider::Claude,
            "codex" => Provider::Codex,
            _ => panic!("unknown fixture provider"),
        };
        let credential = parse(provider, case.body.as_bytes()).unwrap();
        assert_eq!(credential.access_token(), expected.token, "{}", case.name);
        assert_eq!(
            credential.account_id(),
            expected.account_id,
            "{}",
            case.name
        );
        let mut digest = [0u8; 32];
        for (slot, pair) in digest
            .iter_mut()
            .zip(expected.digest_hex.as_bytes().chunks_exact(2))
        {
            *slot = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
        }
        let oracle_identity_digest: [u8; 32] = Sha256::digest(expected.identity.as_bytes()).into();
        assert_eq!(digest, oracle_identity_digest, "{}", case.name);
        assert_eq!(
            credential.account_key(),
            AccountKey::from_digest(digest),
            "{}",
            case.name
        );
    }
}

#[test]
fn errors_are_typed_and_do_not_expose_input() {
    for (provider, body, expected) in [
        (
            Provider::Codex,
            "{secret-token",
            CredentialError::InvalidJson,
        ),
        (Provider::Codex, "null", CredentialError::MissingToken),
        (Provider::Codex, "[]", CredentialError::InvalidJson),
        (
            Provider::Codex,
            r#"{"tokens":null}"#,
            CredentialError::MissingToken,
        ),
        (
            Provider::Codex,
            r#"{"tokens":{"access_token":null}}"#,
            CredentialError::MissingToken,
        ),
        (Provider::Claude, "null", CredentialError::MissingToken),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":null}"#,
            CredentialError::MissingToken,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":[]}"#,
            CredentialError::InvalidJson,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":{"accessToken":42}}"#,
            CredentialError::InvalidJson,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":{"accessToken":"ok","refreshToken":42}}"#,
            CredentialError::InvalidJson,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":{"accessToken":"ok","scopes":[1]}}"#,
            CredentialError::InvalidJson,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":{"accessToken":"ok","expiresAt":"bad"}}"#,
            CredentialError::InvalidJson,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":{"accessToken":"first","ACCESSTOKEN":"second"}}"#,
            CredentialError::AmbiguousField,
        ),
        (
            Provider::Claude,
            r#"{"claudeAiOauth":null,"CLAUDEAIOAUTH":{"accessToken":"second"}}"#,
            CredentialError::AmbiguousField,
        ),
    ] {
        let error = parse(provider, body.as_bytes()).unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("secret-token"));
    }
    let credential = parse(Provider::Codex, br#"{"OPENAI_API_KEY":"secret-token"}"#).unwrap();
    assert_eq!(format!("{credential:?}"), "Credential([redacted])");
    assert_eq!(
        format!("{:?}", credential.account_key()),
        "AccountKey([redacted])"
    );
}

#[test]
fn byte_and_header_limits_are_enforced() {
    assert_eq!(
        parse(Provider::Codex, &vec![b' '; MAX_CREDENTIAL_BYTES + 1]).unwrap_err(),
        CredentialError::TooLarge
    );
    let long_token = "x".repeat(MAX_TOKEN_BYTES + 1);
    let body = format!(r#"{{"OPENAI_API_KEY":"{long_token}"}}"#);
    assert_eq!(
        parse(Provider::Codex, body.as_bytes()).unwrap_err(),
        CredentialError::TooLarge
    );
    let long_id = "i".repeat(MAX_IDENTITY_BYTES + 1);
    let body = format!(r#"{{"tokens":{{"access_token":"valid","account_id":"{long_id}"}}}}"#);
    assert_eq!(
        parse(Provider::Codex, body.as_bytes()).unwrap_err(),
        CredentialError::TooLarge
    );
    for body in [
        r#"{"OPENAI_API_KEY":"bad\rheader"}"#,
        r#"{"OPENAI_API_KEY":"bad token"}"#,
        r#"{"OPENAI_API_KEY":"tökén"}"#,
        r#"{"tokens":{"access_token":"valid","account_id":"bad\nheader"}}"#,
        r#"{"tokens":{"access_token":"valid","account_id":"bad id"}}"#,
        r#"{"tokens":{"access_token":"valid","account_email":"bad\nemail"}}"#,
    ] {
        assert_eq!(
            parse(Provider::Codex, body.as_bytes()).unwrap_err(),
            CredentialError::InvalidHeader
        );
    }
    let unicode_email = parse(
        Provider::Codex,
        br#"{"tokens":{"access_token":"valid","account_email":"\u00e9@example.test"}}"#,
    )
    .unwrap();
    let expected: [u8; 32] = Sha256::digest("em:é@example.test".as_bytes()).into();
    assert_eq!(
        unicode_email.account_key(),
        AccountKey::from_digest(expected)
    );
}

#[test]
fn irrelevant_large_fields_are_streamed_and_discarded() {
    let ignored = "x".repeat(2 * 1024 * 1024);
    let codex = format!(r#"{{"ignored":{{"array":["{ignored}"]}},"OPENAI_API_KEY":"valid"}}"#);
    assert_eq!(
        parse(Provider::Codex, codex.as_bytes())
            .unwrap()
            .access_token(),
        "valid"
    );
    let claude =
        format!(r#"{{"claudeAiOauth":{{"accessToken":"valid","refreshToken":"{ignored}"}}}}"#);
    assert_eq!(
        parse(Provider::Claude, claude.as_bytes())
            .unwrap()
            .access_token(),
        "valid"
    );
}

#[test]
fn same_token_tracks_rotation_independently_of_identity() {
    let a = parse(
        Provider::Codex,
        br#"{"tokens":{"access_token":"first","account_id":"same"}}"#,
    )
    .unwrap();
    let b = parse(
        Provider::Codex,
        br#"{"tokens":{"access_token":"second","account_id":"same"}}"#,
    )
    .unwrap();
    let c = parse(Provider::Codex, br#"{"OPENAI_API_KEY":"first"}"#).unwrap();
    assert!(!a.same_token(&b));
    assert!(a.same_token(&c));
    assert_eq!(a.account_key(), b.account_key());
    assert_ne!(a.account_key(), c.account_key());
}
