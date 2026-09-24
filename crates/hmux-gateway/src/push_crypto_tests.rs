use super::*;
use aes_gcm::aead::Aead;
use p256::ecdsa::{signature::Verifier, VerifyingKey};

const RECEIVER_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const RECEIVER_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const SENDER_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";

fn subscription() -> Subscription {
    Subscription {
        endpoint: "https://FCM.GOOGLEAPIS.COM/push/example".into(),
        keys: push_state::Keys {
            auth: AUTH.into(),
            p256dh: RECEIVER_PUBLIC.into(),
        },
    }
}

fn identity() -> SigningIdentity {
    let scalar = URL_SAFE_NO_PAD.decode(SENDER_PRIVATE).unwrap();
    let secret = SecretKey::from_slice(&scalar).unwrap();
    let public = URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes());
    SigningIdentity::from_parts(&public, SENDER_PRIVATE).unwrap()
}

fn decrypt(body: &[u8]) -> Result<Vec<u8>, Error> {
    let private =
        SecretKey::from_slice(&URL_SAFE_NO_PAD.decode(RECEIVER_PRIVATE).unwrap()).unwrap();
    let sender = PublicKey::from_sec1_bytes(&body[21..86]).unwrap();
    let shared = diffie_hellman(private.to_nonzero_scalar(), sender.as_affine());
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(&URL_SAFE_NO_PAD.decode(RECEIVER_PUBLIC).unwrap());
    info.extend_from_slice(&body[21..86]);
    let mut ikm = [0; 32];
    Hkdf::<Sha256>::new(
        Some(&URL_SAFE_NO_PAD.decode(AUTH).unwrap()),
        shared.raw_secret_bytes(),
    )
    .expand(&info, &mut ikm)
    .unwrap();
    let hkdf = Hkdf::<Sha256>::new(Some(&body[..16]), &ikm);
    let mut cek = [0; 16];
    let mut nonce = [0; 12];
    hkdf.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .unwrap();
    hkdf.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .unwrap();
    let cipher = Aes128Gcm::new_from_slice(&cek).unwrap();
    cipher
        .decrypt(Nonce::from_slice(&nonce), &body[86..])
        .map_err(|_| Error::Invalid)
}

#[test]
fn rfc8291_section_five_exact_body() {
    let scalar = URL_SAFE_NO_PAD.decode(SENDER_PRIVATE).unwrap();
    let ephemeral = SecretKey::from_slice(&scalar).unwrap();
    let salt: [u8; 16] = URL_SAFE_NO_PAD.decode(SALT).unwrap().try_into().unwrap();
    let plaintext = b"When I grow up, I want to be a watermelon";
    let actual = encrypt_record(&subscription(), &ephemeral, &salt, plaintext, false).unwrap();
    let expected = concat!(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
        "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
        "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
    );
    assert_eq!(URL_SAFE_NO_PAD.encode(&actual), expected);
    assert_eq!(
        &*decrypt(&actual).unwrap(),
        b"When I grow up, I want to be a watermelon\x02"
    );
}

#[test]
fn padded_record_limits_and_tampering() {
    let id = identity();
    let login_id = URL_SAFE_NO_PAD.encode([7u8; 32]);
    let prepared = prepare(
        &id,
        &subscription(),
        "https://home.example:443",
        &login_id,
        &vec![b'x'; MAX_PLAINTEXT],
        1_700_000_000,
    )
    .unwrap();
    assert_eq!(prepared.body.len(), 4096);
    assert_eq!(&prepared.body[16..20], &4096u32.to_be_bytes());
    assert_eq!(prepared.body[20], 65);
    assert_eq!(prepared.ttl, "120");
    assert_eq!(prepared.urgency, "normal");
    let plain = decrypt(&prepared.body).unwrap();
    assert_eq!(plain.len(), MAX_PLAINTEXT + 1);
    assert_eq!(plain[MAX_PLAINTEXT], 2);
    let mut altered = prepared.body.to_vec();
    altered[100] ^= 1;
    assert!(decrypt(&altered).is_err());
    assert!(matches!(
        prepare(
            &id,
            &subscription(),
            "https://home.example",
            &login_id,
            &vec![0; MAX_PLAINTEXT + 1],
            1_700_000_000
        ),
        Err(Error::TooLarge)
    ));
}

#[test]
fn independent_salt_and_ephemeral_each_request() {
    let id = identity();
    let login_id = URL_SAFE_NO_PAD.encode([8u8; 32]);
    let a = prepare(
        &id,
        &subscription(),
        "https://home.example",
        &login_id,
        b"{}",
        1_700_000_000,
    )
    .unwrap();
    let b = prepare(
        &id,
        &subscription(),
        "https://home.example",
        &login_id,
        b"{}",
        1_700_000_000,
    )
    .unwrap();
    assert_ne!(&a.body[..16], &b.body[..16]);
    assert_ne!(&a.body[21..86], &b.body[21..86]);
    assert_eq!(a.topic, b.topic);
    assert_eq!(a.endpoint, subscription().endpoint);
    assert_eq!(a.content_encoding, "aes128gcm");
    assert_eq!(a.content_type, "application/octet-stream");
    assert_eq!(decrypt(&a.body).unwrap()[..3], *b"{}\x02");

    let token = a
        .authorization
        .strip_prefix("vapid t=")
        .unwrap()
        .split(", k=")
        .next()
        .unwrap();
    let parts: Vec<_> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["typ"], "JWT");
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["aud"], "https://fcm.googleapis.com");
    assert_eq!(claims["sub"], "https://home.example");
    assert_eq!(claims["exp"], 1_700_043_200i64);
    let key = VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&id.public).unwrap()).unwrap();
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
}

#[test]
fn malformed_inputs_and_mismatched_signing_pair() {
    let id = identity();
    let login_id = URL_SAFE_NO_PAD.encode([9u8; 32]);
    let mut sub = subscription();
    sub.endpoint = "https://localhost/push".into();
    assert!(matches!(
        prepare(&id, &sub, "https://home.example", &login_id, b"{}", 0),
        Err(Error::Invalid)
    ));
    sub = subscription();
    sub.keys.p256dh = "bad".into();
    assert!(matches!(
        prepare(&id, &sub, "https://home.example", &login_id, b"{}", 0),
        Err(Error::Invalid)
    ));
    let public = URL_SAFE_NO_PAD.encode(
        SecretKey::from_slice(&[1u8; 32])
            .unwrap()
            .public_key()
            .to_encoded_point(false)
            .as_bytes(),
    );
    assert!(matches!(
        SigningIdentity::from_parts(&public, SENDER_PRIVATE),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        prepare(
            &id,
            &subscription(),
            "https://home.example/path",
            &login_id,
            b"{}",
            0
        ),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        prepare(
            &id,
            &subscription(),
            "https://home.example",
            "bad",
            b"{}",
            0
        ),
        Err(Error::Invalid)
    ));
}

#[tokio::test]
#[ignore = "the optional external baseline suite (tests/RUST.md) provides the actual Go Web Push sender/validator"]
async fn actual_go_and_rust_push_crypto_interoperate() {
    use hmux_core::command::{CommandRunner, CommandSpec};
    use serde::{Deserialize, Serialize};
    use std::{
        collections::BTreeMap,
        fs,
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    #[derive(Serialize, Deserialize)]
    struct Request {
        endpoint: String,
        headers: BTreeMap<String, String>,
        body: String,
        plaintext: String,
        login_id: String,
        origin: String,
        now: i64,
        subscription: Subscription,
        receiver_private: String,
        vapid_public: String,
        vapid_private: String,
    }
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "hmux-push-crypto-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let fixture = Fixture(root);
    let id = identity();
    let mut sub = subscription();
    sub.endpoint = "https://web.push.apple.com/synthetic".into();
    let login = URL_SAFE_NO_PAD.encode([1; 32]);
    let origin = "https://hmux.example";
    let now = 1_700_000_000;
    let payloads = [
        Vec::new(),
        serde_json::to_vec(&serde_json::json!({
            "type":"codex-complete", "tab_name":"개발 탭",
            "session":{"id":"$42","created_at":1},
            "login_id":login, "event_id":"a".repeat(64)
        }))
        .unwrap(),
        vec![0xff; MAX_PLAINTEXT],
    ];
    let requests: Vec<_> = payloads
        .iter()
        .map(|payload| {
            let p = prepare(&id, &sub, origin, &login, payload, now).unwrap();
            Request {
                endpoint: p.endpoint,
                headers: BTreeMap::from([
                    ("Authorization".into(), p.authorization),
                    ("Content-Encoding".into(), p.content_encoding.into()),
                    ("Content-Type".into(), p.content_type.into()),
                    ("TTL".into(), p.ttl.into()),
                    ("Urgency".into(), p.urgency.into()),
                    ("Topic".into(), p.topic),
                ]),
                body: URL_SAFE_NO_PAD.encode(p.body),
                plaintext: URL_SAFE_NO_PAD.encode(payload),
                login_id: login.clone(),
                origin: origin.into(),
                now,
                subscription: sub.clone(),
                receiver_private: RECEIVER_PRIVATE.into(),
                vapid_public: id.public.clone(),
                vapid_private: SENDER_PRIVATE.into(),
            }
        })
        .collect();
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.0.join("rust-requests.json"))
        .unwrap()
        .write_all(&serde_json::to_vec(&requests).unwrap())
        .unwrap();
    let helper = std::env::var_os("HMUX_GO_PUSH_CRYPTO_HELPER")
        .expect("tests/RUST.md describes the external legacy helper");
    let result = CommandRunner::new(1)
        .unwrap()
        .run(
            CommandSpec::new(helper, 8192, Duration::from_secs(10))
                .arg("-test.run=^TestRustPushCryptoHandoff$")
                .arg("-test.timeout=8s")
                .env("HMUX_RUST_PUSH_CRYPTO", fixture.0.as_os_str()),
        )
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stdout)
        .contains("actual Go sender captured without network"));
    let outputs: Vec<Request> =
        serde_json::from_slice(&fs::read(fixture.0.join("go-requests.json")).unwrap()).unwrap();
    assert_eq!(outputs.len(), requests.len());
    for (request, output) in requests.iter().zip(outputs) {
        assert_eq!(output.endpoint, request.endpoint);
        assert!(output.subscription == request.subscription);
        let body = URL_SAFE_NO_PAD.decode(&output.body).unwrap();
        assert_eq!(body.len(), RECORD_SIZE);
        let padded = decrypt(&body).unwrap();
        let end = padded.iter().rposition(|b| *b != 0).unwrap();
        assert_eq!(padded[end], 2);
        assert_eq!(
            &padded[..end],
            URL_SAFE_NO_PAD.decode(&request.plaintext).unwrap()
        );
        let header = |name: &str| {
            output
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .unwrap()
                .1
                .as_str()
        };
        for name in [
            "Content-Encoding",
            "Content-Type",
            "TTL",
            "Urgency",
            "Topic",
        ] {
            assert_eq!(header(name), request.headers[name]);
        }
        let (token, public) = header("Authorization")
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(public, request.vapid_public);
        let parts: Vec<_> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let head: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(head, serde_json::json!({"alg":"ES256","typ":"JWT"}));
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["sub"], request.origin);
        let exp = claims["exp"].as_i64().unwrap();
        assert!(exp > output.now && exp <= output.now + 12 * 60 * 60);
        let key =
            VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&request.vapid_public).unwrap())
                .unwrap();
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
    }
}
