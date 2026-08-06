use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
pub const DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const TOKEN_LIFETIME_SECS: u64 = 3600;

#[derive(Clone)]
pub struct ServiceAccount {
    pub client_email: String,
    pub private_key: String,
    pub token_uri: String,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .field("token_uri", &self.token_uri)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct SaFile {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    DEFAULT_TOKEN_URI.to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JwtClaims {
    pub iss: String,
    pub scope: String,
    pub aud: String,
    pub iat: u64,
    pub exp: u64,
}

impl ServiceAccount {
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading service account file {}", path.display()))?;
        let f: SaFile = serde_json::from_str(&raw)
            .with_context(|| format!("parsing service account file {}", path.display()))?;
        Ok(Self {
            client_email: f.client_email,
            private_key: f.private_key,
            token_uri: f.token_uri,
        })
    }

    /// Builds the JWT assertion for the OAuth2 JWT-bearer grant (no network).
    pub fn build_jwt(&self) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_secs();
        // 60s leeway for clock skew against Google's token endpoint.
        let iat = now.saturating_sub(60);
        let claims = JwtClaims {
            iss: self.client_email.clone(),
            scope: DRIVE_READONLY_SCOPE.to_string(),
            aud: self.token_uri.clone(),
            iat,
            exp: iat + TOKEN_LIFETIME_SECS,
        };
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(self.private_key.as_bytes())
            .context("service account private key is not a valid RSA PEM")?;
        jsonwebtoken::encode(&header, &claims, &key).context("signing JWT")
    }

    /// Exchanges the JWT for an OAuth2 access token (network).
    pub async fn fetch_access_token(&self, client: &reqwest::Client) -> Result<String> {
        let jwt = self.build_jwt()?;
        let resp = client
            .post(&self.token_uri)
            .form(&[
                ("grant_type", JWT_BEARER_GRANT),
                ("assertion", jwt.as_str()),
            ])
            .send()
            .await
            .context("token endpoint request failed")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("(body unreadable: {e})"));
        if !status.is_success() {
            anyhow::bail!("token endpoint returned HTTP {status}: {body}");
        }
        let json: serde_json::Value =
            serde_json::from_str(&body).context("token endpoint returned invalid JSON")?;
        json.get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .with_context(|| {
                format!(
                    "token endpoint response missing access_token: {}",
                    &body[..body.len().min(200)]
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only RSA private key: PKCS#8 PEM. Generate with
    // `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out /tmp/wsb-test-key.pem`
    // (or `openssl genrsa -traditional` — jsonwebtoken 9.x accepts both PKCS#1 and PKCS#8).
    const TEST_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCvEQho2FdHRsLlCru28OOvGkpMO0gemyj7lPjZDYk0n2nuZIEbuHKA5k5a8ccpgHIJGBifVcZ2ZMAA/PMP9Pb7Pt5TolVYmz/mHbdoZ0NTpRjv0NkndkmI3pFJwe2BACNnhj93aC+m468GSjaZs0C+v/VC8699mVraEeKciqeJy9UzC9F7i2U9z8HqXcct3cu9pa9Kv3mouuzQPCOAq+N3HOIaw35PUBSRDoG5WTvShWoXyW71Yo3MDWRheqVX+4cs2DKue+pk7yL7haT6RKw37I3zq+tUCsuB7OQHTtlDvSga5cglRmvNqzwcPMiAdMK5aQS1YZDF2IEnAJ0vVRctAgMBAAECggEAJaQ0qfBsUbnEAYHdiy//0KBHPd1YPCZx+SgWmnrX1734BadMAFUYH6GFUvYd8803l7972dSUU9QFWaEJtRJfgXWK0bI7hg35fwXAL/1WA1fiBPxjmKHNHVX3qMN/COfp9OIvZsH6zvgxI5nU5Bbf8rOs7TyerNOKro0+a5i/fbe/rzr1TxOSJfUlWIGrXHik548E4CUFvOiDsyxqWTN3azfMgSSfgriRGS2uxC1qsRMCNNTFuhIKt9T2S8TfthZuvWSMlfmbkhnDz57+LHjDVBIwm8y3SpIJDf0tpqFU4tDbWEkOZx5j5eVFMQ7kcWH0aZNkFazxUwUZ1r0zdbnLcQKBgQDszU/Iuy0Zr5bZ/HA7iM5SE97T/OdWUPWQ4Y2WzBJtE8eSFUcbPLaG+6kYVy+P2X1iJLmA+LMbv6MBqtVf1rRAgV301PevaPOoYfploKDl3ivrizli4KR0Ud/hrWRLXZyEdKtaDI4uHoI9rQULAEEq1RTy7rOgIYBfZ6+VVyPTHQKBgQC9Qm/4tYZPH/pOFf7hJrKbM21iRmuWBUqVH8gK4TsV7zjZdnF9Bd9JeEvNIjE/+je6j0Yva2DctNwJtShGujnLZOyQdLmw//bGch/cFtXOgGvW/Si2i4wZWtb0UgqATp8UrLO/dc7kByaA3XuPCsB1DQ77FqWZ6XVluk03aGeHUQKBgQDfbtXCFAJ5Avm2Uv9e3TW3sjIFGdL52cfq3TeouoMEUq5ywwrlw0KCWMBzTAh/lXo9+WLjM0Zkf0yCDTvpgv9vAeGyWqQd6UxGa7RE4ewPGLOeOy55gncJnhs9qEpC5mABhsgLXl9lWroPEcr5V1Ml5AoxMlNgW1vyKTY+FguibQKBgQCydv6tMUdIP6hBj947o8kSLsl7vVngKncs7b4t/DtCMMWT0mur8Cig2C3qbs6wPJvmcQpG1uOM24MOKGSlZR/wmue0RE2CCaxDbwR5/pJ42oJWRXzpvedLVWyTEPXUDc9WqJAK/+UrA08cfz3vIb1f4wN4Y9+ephXM6oO7ttjBMQKBgGymw/s2pTYAv6f5q2SboERmELmDFcxJloE/StKNp2iFGbek82ZgbbEK/2x7tVKJ6Ef1EKtAOqKc4uPFdW0FGUbOG/XR/9Kd0XN2YbIKjDfX8dKE2I0umzs5eUhLk00+bpn16vX/Dk5XlhFIURfYFTNonW4ek0jOn7s1/oZ+pt1o\n-----END PRIVATE KEY-----\n";

    // Matching public key (PKCS#8 SPKI PEM) for the private key above, derived with
    // `openssl pkey -in /tmp/wsb-test-key.pem -pubout`. jsonwebtoken's
    // `DecodingKey::from_rsa_pem` requires a *public* key PEM for verification —
    // passing a private key PEM produces InvalidSignature.
    const TEST_PUB_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEArxEIaNhXR0bC5Qq7tvDjrxpKTDtIHpso+5T42Q2JNJ9p7mSBG7hygOZOWvHHKYByCRgYn1XGdmTAAPzzD/T2+z7eU6JVWJs/5h23aGdDU6UY79DZJ3ZJiN6RScHtgQAjZ4Y/d2gvpuOvBko2mbNAvr/1QvOvfZla2hHinIqnicvVMwvRe4tlPc/B6l3HLd3LvaWvSr95qLrs0DwjgKvjdxziGsN+T1AUkQ6BuVk70oVqF8lu9WKNzA1kYXqlV/uHLNgyrnvqZO8i+4Wk+kSsN+yN86vrVArLgezkB07ZQ70oGuXIJUZrzas8HDzIgHTCuWkEtWGQxdiBJwCdL1UXLQIDAQAB\n-----END PUBLIC KEY-----\n";

    const SA_JSON: &str = r#"{
        "type": "service_account",
        "project_id": "wsb-test",
        "private_key_id": "abc123",
        "private_key": "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCx\nfake\n-----END PRIVATE KEY-----\n",
        "client_email": "wsb-test@wsb-test.iam.gserviceaccount.com",
        "client_id": "1234567890",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
        "client_x509_cert_url": "https://www.googleapis.com/robot/v1/metadata/x509/wsb-test%40wsb-test.iam.gserviceaccount.com"
    }"#;

    fn write_sa_json(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("sa.json");
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn test_from_file_parses_and_decodes_escaped_newlines() {
        let dir = std::env::temp_dir().join(format!("wsb-sa-test-{}", std::process::id()));
        let p = write_sa_json(&dir, SA_JSON);
        let sa = ServiceAccount::from_file(&p).unwrap();
        assert_eq!(sa.client_email, "wsb-test@wsb-test.iam.gserviceaccount.com");
        assert_eq!(sa.token_uri, "https://oauth2.googleapis.com/token");
        // The JSON's literal `\n` escapes must decode into real newlines.
        assert!(sa.private_key.contains('\n'));
        assert!(!sa.private_key.contains("\\n"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_from_file_missing_errors() {
        let dir = std::env::temp_dir().join(format!("wsb-sa-test-{}", std::process::id()));
        let p = dir.join("does-not-exist.json");
        assert!(ServiceAccount::from_file(&p).is_err());
    }

    #[test]
    fn test_build_jwt_claims() {
        let sa = ServiceAccount {
            client_email: "wsb-test@wsb-test.iam.gserviceaccount.com".into(),
            private_key: TEST_PEM.into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        };
        let jwt = sa.build_jwt().unwrap();
        let data = jsonwebtoken::decode::<JwtClaims>(
            &jwt,
            &jsonwebtoken::DecodingKey::from_rsa_pem(TEST_PUB_PEM.as_bytes()).unwrap(),
            &{
                let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
                v.set_audience(&["https://oauth2.googleapis.com/token"]);
                v
            },
        )
        .unwrap();
        assert_eq!(data.claims.iss, "wsb-test@wsb-test.iam.gserviceaccount.com");
        assert_eq!(
            data.claims.scope,
            "https://www.googleapis.com/auth/drive.readonly"
        );
        assert_eq!(data.claims.exp - data.claims.iat, 3600);
    }
}
