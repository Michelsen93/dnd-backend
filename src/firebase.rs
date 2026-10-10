//! Firebase Authentication: verify ID tokens from the web app, and delete Firebase users when an
//! account is deleted. Tokens are RS256 JWTs signed by Google; see
//! <https://firebase.google.com/docs/auth/admin/verify-id-tokens#verify_id_tokens_using_a_third-party_jwt_library>.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::error::ApiError;

const JWKS_URL: &str =
    "https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com";
const METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

/// The identity we trust after verifying a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirebaseIdentity {
    pub uid: String,
    pub email: String,
    pub email_verified: bool,
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    auth_time: i64,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct KeyCache {
    keys: HashMap<String, DecodingKey>,
    expires_at: Instant,
}

pub struct FirebaseVerifier {
    project_id: String,
    client: reqwest::Client,
    cache: RwLock<KeyCache>,
    /// Tests inject keys and never fetch.
    fetch_keys: bool,
}

impl FirebaseVerifier {
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            cache: RwLock::new(KeyCache {
                keys: HashMap::new(),
                expires_at: Instant::now(),
            }),
            fetch_keys: true,
        }
    }

    /// For tests: a verifier with fixed signing keys (kid → key) that never goes to the network.
    pub fn with_static_keys(
        project_id: impl Into<String>,
        keys: HashMap<String, DecodingKey>,
    ) -> Self {
        let mut verifier = Self::new(project_id);
        verifier.fetch_keys = false;
        verifier.cache = RwLock::new(KeyCache {
            keys,
            expires_at: Instant::now() + Duration::from_secs(365 * 24 * 3600),
        });
        verifier
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub async fn verify(&self, token: &str) -> Result<FirebaseIdentity, ApiError> {
        let invalid = || ApiError::unauthorized("Invalid sign-in token");
        let header = decode_header(token).map_err(|_| invalid())?;
        if header.alg != Algorithm::RS256 {
            return Err(invalid());
        }
        let kid = header.kid.ok_or_else(invalid)?;
        let key = self.key(&kid).await?.ok_or_else(invalid)?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.project_id.as_str()]);
        validation.set_issuer(&[format!(
            "https://securetoken.google.com/{}",
            self.project_id
        )]);
        validation.set_required_spec_claims(&["exp", "iat", "aud", "iss", "sub"]);
        validation.leeway = 60;
        let data = decode::<Claims>(token, &key, &validation).map_err(|_| invalid())?;
        let claims = data.claims;

        let now = chrono::Utc::now().timestamp();
        if claims.sub.is_empty() || claims.sub.len() > 128 || claims.auth_time > now + 60 {
            return Err(invalid());
        }
        let email = claims
            .email
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .ok_or_else(|| ApiError::bad_request("This sign-in method has no email address"))?;
        Ok(FirebaseIdentity {
            uid: claims.sub,
            email,
            email_verified: claims.email_verified.unwrap_or(false),
        })
    }

    async fn key(&self, kid: &str) -> Result<Option<DecodingKey>, ApiError> {
        {
            let cache = self.cache.read().await;
            if cache.expires_at > Instant::now() || !self.fetch_keys {
                if let Some(key) = cache.keys.get(kid) {
                    return Ok(Some(key.clone()));
                }
                if !self.fetch_keys {
                    return Ok(None);
                }
            }
        }
        self.refresh_keys().await?;
        Ok(self.cache.read().await.keys.get(kid).cloned())
    }

    async fn refresh_keys(&self) -> Result<(), ApiError> {
        let response = self.client.get(JWKS_URL).send().await.map_err(upstream)?;
        let max_age = response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_max_age)
            .unwrap_or(3600);
        let set: JwkSet = response.json().await.map_err(upstream)?;
        let keys = set
            .keys
            .into_iter()
            .filter_map(|k| {
                DecodingKey::from_rsa_components(&k.n, &k.e)
                    .ok()
                    .map(|d| (k.kid, d))
            })
            .collect();
        *self.cache.write().await = KeyCache {
            keys,
            expires_at: Instant::now() + Duration::from_secs(max_age),
        };
        Ok(())
    }

    /// Delete the Firebase Auth user (GDPR erasure). Uses the Cloud Run service account through the
    /// metadata server; it needs the "Firebase Authentication Admin" role.
    pub async fn delete_user(&self, uid: &str) -> Result<(), ApiError> {
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
        }
        let token: Token = self
            .client
            .get(METADATA_TOKEN_URL)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(upstream)?
            .error_for_status()
            .map_err(upstream)?
            .json()
            .await
            .map_err(upstream)?;
        self.client
            .post(format!(
                "https://identitytoolkit.googleapis.com/v1/projects/{}/accounts:delete",
                self.project_id
            ))
            .bearer_auth(token.access_token)
            .json(&serde_json::json!({ "localId": uid }))
            .send()
            .await
            .map_err(upstream)?
            .error_for_status()
            .map_err(upstream)?;
        Ok(())
    }
}

fn upstream(error: reqwest::Error) -> ApiError {
    tracing::error!(%error, "firebase request failed");
    ApiError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        "The sign-in service is unavailable, try again",
    )
}

fn parse_max_age(header: &str) -> Option<u64> {
    header
        .split(',')
        .find_map(|part| part.trim().strip_prefix("max-age="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::parse_max_age;

    #[test]
    fn parses_max_age() {
        assert_eq!(
            parse_max_age("public, max-age=19302, must-revalidate"),
            Some(19302)
        );
        assert_eq!(parse_max_age("no-cache"), None);
    }
}
