// src/oidc.rs
//
// OAuth2 / OIDC bearer-token validation for the MAPI over HTTP Hybrid Modern
// Authentication (HMA) path.
//
// Phase 0 surface:
//   * `TokenVerifier` validates a presented `Authorization: Bearer <jwt>`
//     token against a configured Entra ID issuer + audience, enforcing
//     signature (RS256 / ES256), `exp`, `nbf`, `iss`, and `aud`.
//   * Decoding keys are fetched from the issuer's JWKS endpoint on first use
//     and cached by `kid`, with a TTL refresh and a fail-closed fallback if
//     the JWKS fetch fails (no signing key ⇒ reject, never accept unsigned).
//   * On success the verifier yields a `Principal` with `upn`, `oid`,
//     `tid`, and the raw token — the bridge to a Stalwart mailbox lives in
//     `mapi::logon` Phase 1 and reuses a deterministic `upn` → mailbox
//     mapping; Phase 0 stores the principal on the session only.
//
// The validator never falls back to silent-acceptance: a JWKS fetch error or
// an unknown `kid` is a 401, not a defer. This is the correct HMA posture —
// New Outlook for Windows does not fall back to Basic auth when HMA is
// configured, so a verification failure must surface as a definitive
// authentication error.
//
// Code priority note: `jsonwebtoken` is a well-audited, widely-deployed crate
// for RS256/ES256 verification and claims validation. We use it rather than
// hand-rolling JOSE — hand-rolled JWT validation is a known foot-gun area.

use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header, errors::Error as JwtError,
};
use parking_lot::RwLock;
use serde::Deserialize;

/// The minimum required claims Outlook's New-OAuth HMA path needs. We resolve
/// the user's mailbox handle from `preferred_username` (UPN) first, then `oid`.
///
/// A custom `Debug` impl is provided so logging the principal (or any error
/// wrapping one) never emits the bearer token, which is a secret. The default
/// derived `Debug` would print `raw_token` verbatim.
#[derive(Clone)]
pub struct Principal {
    /// User Principal Name (typically the email address).
    pub upn: Option<String>,
    /// Object identifier — stable per-user id from Entra ID.
    pub oid: Option<String>,
    /// Tenant identifier.
    pub tid: Option<String>,
    /// The raw bearer token, surfaced for downstream backend bridging.
    pub raw_token: String,
}

impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the raw token — it is a credential. Emit a fixed
        // "**redacted**" sentinel so downstream `[trace] ?principal` calls
        // (and any error wrapping a Principal) cannot leak it.
        f.debug_struct("Principal")
            .field("upn", &self.upn)
            .field("oid", &self.oid)
            .field("tid", &self.tid)
            .field("raw_token", &"**redacted**")
            .finish()
    }
}

/// Errors returned by the verifier. The HTTP layer maps these to a single
/// deterministic 401 with no internal-detail leak.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("malformed token")]
    Malformed,
    #[error("no kid in token header")]
    NoKid,
    #[error("unknown kid")]
    UnknownKid,
    #[error("jwks fetch failed")]
    JwksFetch,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid claims: {0}")]
    InvalidClaims(String),
}

impl From<JwtError> for VerifyError {
    fn from(e: JwtError) -> Self {
        match e.kind() {
            jsonwebtoken::errors::ErrorKind::InvalidSignature
            | jsonwebtoken::errors::ErrorKind::InvalidRsaKey(_) => Self::InvalidSignature,
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                Self::InvalidClaims("token expired".into())
            }
            jsonwebtoken::errors::ErrorKind::ImmatureSignature => {
                Self::InvalidClaims("token not yet valid (nbf)".into())
            }
            jsonwebtoken::errors::ErrorKind::InvalidIssuer => {
                Self::InvalidClaims("invalid issuer".into())
            }
            jsonwebtoken::errors::ErrorKind::InvalidAudience => {
                Self::InvalidClaims("invalid audience".into())
            }
            jsonwebtoken::errors::ErrorKind::InvalidToken => Self::Malformed,
            _ => Self::Malformed,
        }
    }
}

/// The published claims Outlook carries in an Office 365 Exchange Online
/// token. `iss`/`aud`/`exp`/`nbf` are validated by `jsonwebtoken`'s
/// `Validation` config (we set issuer + audience, and the crate validates
/// exp/nbf by default), so this struct only captures the claims we surface
/// on the returned `Principal`.
#[derive(Debug, Clone, Deserialize)]
struct EntraClaims {
    #[serde(default)]
    upn: Option<String>,
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    tid: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

/// Thread-safe HMA token verifier with a JWKS cache keyed by `kid`.
#[derive(Clone)]
pub struct TokenVerifier {
    issuer: String,
    audience: String,
    http: reqwest::Client,
    cache: Arc<RwLock<Option<JwkSet>>>,
    cache_ttl: Duration,
    last_fetch: Arc<RwLock<Option<std::time::Instant>>>,
}

impl TokenVerifier {
    /// Construct a verifier for `issuer` and `audience`. `issuer` is the
    /// Entra ID issuer (e.g. `https://login.microsoftonline.com/<tid>/v2.0`)
    /// and `audience` is the Office 365 Exchange Online resource
    /// (`00000002-0000-0ff1-ce00-000000000000` for HMA, expressed as a URL
    /// or GUID per the JWT `aud` claim Outlook sends).
    pub fn new(issuer: String, audience: String) -> Self {
        Self {
            issuer,
            audience,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            cache: Arc::new(RwLock::new(None)),
            cache_ttl: Duration::from_secs(15 * 60),
            last_fetch: Arc::new(RwLock::new(None)),
        }
    }

    fn jwks_uri(&self) -> String {
        // Microsoft Entra ID publishes at the v2.0 discovery endpoint.
        // For a tenant-specific issuer `<iss>/`, append `discovery/v2.0/keys`.
        let base = self.issuer.trim_end_matches('/');
        format!("{base}/discovery/v2.0/keys")
    }

    async fn refresh_if_needed(&self) -> Result<(), VerifyError> {
        if let Some(ref last) = *self.last_fetch.read()
            && last.elapsed() < self.cache_ttl
            && self.cache.read().is_some()
        {
            return Ok(());
        }
        let uri = self.jwks_uri();
        let fetch = self.http.get(&uri).send().await;
        let resp = match fetch {
            Ok(r) => r,
            Err(_) => return Err(VerifyError::JwksFetch),
        };
        if !resp.status().is_success() {
            return Err(VerifyError::JwksFetch);
        }
        let body = resp.bytes().await.map_err(|_| VerifyError::JwksFetch)?;
        let jwks: JwkSet = serde_json::from_slice(&body).map_err(|_| VerifyError::JwksFetch)?;
        *self.cache.write() = Some(jwks);
        *self.last_fetch.write() = Some(std::time::Instant::now());
        Ok(())
    }

    fn find_key(&self, kid: &str, alg: Algorithm) -> Result<DecodingKey, VerifyError> {
        let guard = self.cache.read();
        let Some(jwks) = guard.as_ref() else {
            return Err(VerifyError::UnknownKid);
        };
        // Find a JWK with matching kid. `JwkSet::find` matches on
        // `common.key_id`.
        let jwk = jwks.find(kid).ok_or(VerifyError::UnknownKid)?;
        // Require the JWK's `alg` to match the token's algorithm, when the
        // JWK advertises one. A mismatched alg is a signature-attack vector.
        if let Some(key_alg) = &jwk.common.key_algorithm
            && algorithm_to_key_algorithm(alg).as_ref() != Some(key_alg)
        {
            return Err(VerifyError::InvalidSignature);
        }
        decoding_key_from_jwk(jwk).ok_or(VerifyError::InvalidSignature)
    }

    /// Verify `bearer` and return the authenticated principal.
    pub async fn verify(&self, bearer: &str) -> Result<Principal, VerifyError> {
        // Strip the "Bearer " prefix if present.
        let token = bearer.strip_prefix("Bearer ").unwrap_or(bearer).trim();
        if token.is_empty() {
            return Err(VerifyError::Malformed);
        }
        let header = decode_header(token).map_err(|_| VerifyError::Malformed)?;
        let alg = header.alg;
        // Only accept asymmetric algorithms on the HMA path.
        if !matches!(
            alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::ES256
                | Algorithm::ES384
        ) {
            return Err(VerifyError::Malformed);
        }
        let Some(kid) = header.kid else {
            return Err(VerifyError::NoKid);
        };
        self.refresh_if_needed().await?;
        let key = self.find_key(&kid, alg)?;
        let mut validation = Validation::new(alg);
        validation.set_issuer(&[&self.issuer]);
        // Audience can be either a string or an array of strings; accept
        // either form provided the configured audience is present.
        validation.set_audience(&[&self.audience]);
        // jsonwebtoken validates exp/nbf by default; we leave them on.
        let token_data =
            decode::<EntraClaims>(token, &key, &validation).map_err(VerifyError::from)?;
        let claims = token_data.claims;
        let upn = claims
            .preferred_username
            .clone()
            .or_else(|| claims.upn.clone());
        Ok(Principal {
            upn,
            oid: claims.oid,
            tid: claims.tid,
            raw_token: token.to_string(),
        })
    }
}

fn decoding_key_from_jwk(jwk: &Jwk) -> Option<DecodingKey> {
    match &jwk.algorithm {
        AlgorithmParameters::RSA(rsa) => DecodingKey::from_rsa_components(&rsa.n, &rsa.e).ok(),
        AlgorithmParameters::EllipticCurve(ec) => {
            DecodingKey::from_ec_components(&ec.x, &ec.y).ok()
        }
        // Symmetric / octet keys are not accepted on the HMA path (tokens are
        // always asymmetrically signed by Entra ID).
        _ => None,
    }
}

/// Map a `jsonwebtoken::Algorithm` (the token's `alg` header) to the
/// `KeyAlgorithm` advertised in a JWK's `alg` field. Returns `None` if the
/// token algorithm is not a public-key algorithm (HMAC variants), which the
/// caller already rejects earlier.
fn algorithm_to_key_algorithm(alg: Algorithm) -> Option<jsonwebtoken::jwk::KeyAlgorithm> {
    use jsonwebtoken::jwk::KeyAlgorithm as Ka;
    Some(match alg {
        Algorithm::RS256 => Ka::RS256,
        Algorithm::RS384 => Ka::RS384,
        Algorithm::RS512 => Ka::RS512,
        Algorithm::ES256 => Ka::ES256,
        Algorithm::ES384 => Ka::ES384,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_error_kinds_distinguishable() {
        let e = VerifyError::InvalidClaims("issuer".into());
        let malformed = VerifyError::Malformed;
        let kid = VerifyError::UnknownKid;
        assert!(matches!(e, VerifyError::InvalidClaims(_)));
        assert_eq!(malformed.to_string(), "malformed token");
        assert_eq!(kid.to_string(), "unknown kid");
    }

    #[tokio::test]
    async fn rejects_empty_token() {
        let v = TokenVerifier::new("https://login.test/v2.0".into(), "aud".into());
        let err = v.verify("").await.unwrap_err();
        assert!(matches!(err, VerifyError::Malformed));
    }

    #[tokio::test]
    async fn rejects_nongarbage_token() {
        let v = TokenVerifier::new("https://login.test/v2.0".into(), "aud".into());
        // Single dot only — decode_header will reject this.
        let err = v.verify("not-a-jwt").await.unwrap_err();
        assert!(matches!(err, VerifyError::Malformed));
    }

    #[test]
    fn jwks_uri_well_formed() {
        let v = TokenVerifier::new(
            "https://login.microsoftonline.com/abc/v2.0/".into(),
            "aud".into(),
        );
        assert_eq!(
            v.jwks_uri(),
            "https://login.microsoftonline.com/abc/v2.0/discovery/v2.0/keys"
        );
    }
}
