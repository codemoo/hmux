//! Synchronous Web Push request preparation. The caller must use bounded
//! blocking admission, then recheck login authorization and the exact current
//! subscription before network delivery. This module performs no persistence or
//! network operations; fresh entropy comes from the operating system.

use crate::push_state::{self, Subscription};
use aes_gcm::{aead::AeadInPlace, aead::KeyInit, Aes128Gcm, Nonce};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use bytes::Bytes;
use hkdf::Hkdf;
use p256::{
    ecdh::diffie_hellman,
    ecdsa::{signature::Signer, Signature, SigningKey},
    elliptic_curve::{
        sec1::ToEncodedPoint,
        zeroize::{Zeroize, Zeroizing},
    },
    PublicKey, SecretKey,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const RECORD_SIZE: usize = 4096;
const HEADER_SIZE: usize = 16 + 4 + 1 + 65;
const TAG_SIZE: usize = 16;
pub const MAX_PLAINTEXT: usize = RECORD_SIZE - HEADER_SIZE - TAG_SIZE - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    TooLarge,
    Unavailable,
}

/// A checked VAPID pair. The scalar is erased when this value is dropped.
pub struct SigningIdentity {
    public: String,
    scalar: [u8; 32],
}

impl Drop for SigningIdentity {
    fn drop(&mut self) {
        self.scalar.zeroize();
    }
}

impl SigningIdentity {
    pub fn from_parts(public: &str, private: &str) -> Result<Self, Error> {
        let encoded_public = decode_canonical(public, 65)?;
        if encoded_public.first() != Some(&4) {
            return Err(Error::Invalid);
        }
        let scalar = Zeroizing::new(decode_canonical(private, 32)?);
        let secret = SecretKey::from_slice(&scalar).map_err(|_| Error::Invalid)?;
        if secret.public_key().to_encoded_point(false).as_bytes() != encoded_public {
            return Err(Error::Invalid);
        }
        let mut fixed = [0; 32];
        fixed.copy_from_slice(&scalar);
        Ok(Self {
            public: public.to_owned(),
            scalar: fixed,
        })
    }
}

/// Exact body and headers for one HTTP POST. The endpoint and authorization
/// value may contain private subscription material; this type has no Debug.
pub struct Prepared {
    pub endpoint: String,
    pub authorization: String,
    pub content_encoding: &'static str,
    pub content_type: &'static str,
    pub ttl: &'static str,
    pub urgency: &'static str,
    pub topic: String,
    pub body: Bytes,
}

/// `now_unix` is a trusted Unix second. `payload` is already serialized JSON.
/// Every call obtains a new salt and an independent one-use ECDH scalar.
pub fn prepare(
    identity: &SigningIdentity,
    subscription: &Subscription,
    subject_origin: &str,
    login_id: &str,
    payload: &[u8],
    now_unix: i64,
) -> Result<Prepared, Error> {
    if payload.len() > MAX_PLAINTEXT {
        return Err(Error::TooLarge);
    }
    push_state::validate_subscription(subscription).map_err(|_| Error::Invalid)?;
    if !push_state::valid_id(login_id) || !valid_subject(subject_origin) {
        return Err(Error::Invalid);
    }
    let expiration = now_unix
        .checked_add(12 * 60 * 60)
        .filter(|_| now_unix >= 0)
        .ok_or(Error::Invalid)?;
    let authority = subscription.endpoint[8..]
        .split(['/', '?'])
        .next()
        .ok_or(Error::Invalid)?;
    let audience = format!("https://{}", authority.to_ascii_lowercase());

    let mut salt = [0; 16];
    getrandom::fill(&mut salt).map_err(|_| Error::Unavailable)?;
    let ephemeral = random_secret()?;
    let body = encrypt_record(subscription, &ephemeral, &salt, payload, true)?;

    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","typ":"JWT"}"#);
    let claims =
        serde_json::to_vec(&json!({"aud": audience, "exp": expiration, "sub": subject_origin}))
            .map_err(|_| Error::Unavailable)?;
    let signing_input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims));
    let signing_key = SigningKey::from_slice(&identity.scalar).map_err(|_| Error::Unavailable)?;
    let signature: Signature = signing_key
        .try_sign(signing_input.as_bytes())
        .map_err(|_| Error::Unavailable)?;
    let token = format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    let authorization = format!("vapid t={token}, k={}", identity.public);

    let mut topic_hash = Sha256::new();
    topic_hash.update(login_id.as_bytes());
    topic_hash.update(payload);
    let digest = topic_hash.finalize();
    let topic = URL_SAFE_NO_PAD.encode(&digest[..24]);
    Ok(Prepared {
        endpoint: subscription.endpoint.clone(),
        authorization,
        content_encoding: "aes128gcm",
        content_type: "application/octet-stream",
        ttl: "120",
        urgency: "normal",
        topic,
        body,
    })
}

fn decode_canonical(value: &str, count: usize) -> Result<Vec<u8>, Error> {
    if value.len() > (count * 4).div_ceil(3) {
        return Err(Error::Invalid);
    }
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| Error::Invalid)?;
    if decoded.len() != count || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(Error::Invalid);
    }
    Ok(decoded)
}

fn valid_subject(origin: &str) -> bool {
    if origin.len() > 2048
        || !origin.is_ascii()
        || origin.bytes().any(|b| b <= 0x20 || b == 0x7f || b == b'\\')
    {
        return false;
    }
    let Some(host) = origin.strip_prefix("https://") else {
        return false;
    };
    if host.is_empty() || host.contains(['/', '?', '#', '@']) {
        return false;
    }
    let Ok(authority) = host.parse::<http::uri::Authority>() else {
        return false;
    };
    let suffix = &host[authority.host().len()..];
    !authority.host().is_empty()
        && (suffix.is_empty()
            || suffix
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .is_some())
}

fn random_secret() -> Result<SecretKey, Error> {
    for _ in 0..16 {
        let mut scalar = Zeroizing::new([0; 32]);
        getrandom::fill(&mut *scalar).map_err(|_| Error::Unavailable)?;
        let result = SecretKey::from_slice(&*scalar);
        if let Ok(secret) = result {
            return Ok(secret);
        }
    }
    Err(Error::Unavailable)
}

// `pad=false` exists only for the RFC 8291 section 5 vector test.
fn encrypt_record(
    subscription: &Subscription,
    ephemeral: &SecretKey,
    salt: &[u8; 16],
    payload: &[u8],
    pad: bool,
) -> Result<Bytes, Error> {
    if payload.len() > MAX_PLAINTEXT {
        return Err(Error::TooLarge);
    }
    let auth = Zeroizing::new(decode_canonical(&subscription.keys.auth, 16)?);
    let public_bytes = decode_canonical(&subscription.keys.p256dh, 65)?;
    if public_bytes.first() != Some(&4) {
        return Err(Error::Invalid);
    }
    let receiver = PublicKey::from_sec1_bytes(&public_bytes).map_err(|_| Error::Invalid)?;
    let sender_bytes = ephemeral.public_key().to_encoded_point(false);
    let shared = diffie_hellman(ephemeral.to_nonzero_scalar(), receiver.as_affine());
    let mut info = Vec::with_capacity(14 + 65 + 65);
    info.extend_from_slice(b"WebPush: info\0");
    info.extend_from_slice(&public_bytes);
    info.extend_from_slice(sender_bytes.as_bytes());
    let mut ikm = [0; 32];
    Hkdf::<Sha256>::new(Some(&auth), shared.raw_secret_bytes())
        .expand(&info, &mut ikm)
        .map_err(|_| Error::Unavailable)?;
    let hkdf = Hkdf::<Sha256>::new(Some(salt), &ikm);
    ikm.zeroize();
    let mut cek = [0; 16];
    let mut nonce = [0; 12];
    hkdf.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .map_err(|_| Error::Unavailable)?;
    hkdf.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|_| Error::Unavailable)?;
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| Error::Unavailable)?;
    cek.zeroize();
    let plain_len = if pad {
        RECORD_SIZE - HEADER_SIZE - TAG_SIZE
    } else {
        payload.len() + 1
    };
    let mut body = vec![0; HEADER_SIZE + plain_len + TAG_SIZE];
    body[..16].copy_from_slice(salt);
    body[16..20].copy_from_slice(&(RECORD_SIZE as u32).to_be_bytes());
    body[20] = 65;
    body[21..HEADER_SIZE].copy_from_slice(sender_bytes.as_bytes());
    let (head_and_record, tag_slot) = body.split_at_mut(HEADER_SIZE + plain_len);
    let record = &mut head_and_record[HEADER_SIZE..];
    record[..payload.len()].copy_from_slice(payload);
    record[payload.len()] = 0x02;
    let encrypted = cipher.encrypt_in_place_detached(Nonce::from_slice(&nonce), b"", record);
    nonce.zeroize();
    match encrypted {
        Ok(tag) => tag_slot.copy_from_slice(&tag),
        Err(_) => {
            body.zeroize();
            return Err(Error::Unavailable);
        }
    }
    Ok(Bytes::from(body))
}

#[cfg(test)]
#[path = "push_crypto_tests.rs"]
mod tests;
