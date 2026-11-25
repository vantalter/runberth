//! Just enough of GitHub's runner broker protocol to wait for work on a
//! registered runner's behalf, using that runner's own credentials.
//!
//! This follows GitHub's open-source runner (actions/runner): the listener
//! signs a JWT with the runner's RSA key, exchanges it for an access token,
//! opens a broker session, and long-polls `message`. Runberth never runs or
//! acknowledges a job and never deletes a message; it closes its session and
//! leaves the message for the real runner. GitHub's runner relies on the same
//! redelivery when it skips a job it cannot take.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

use crate::arch::Arch;
use crate::bail;
use crate::error::{Context, Error, Result};
use crate::registry::read_record;
use crate::system;

/// How long the broker may hold a message request open.
const LONG_POLL: Duration = Duration::from_secs(120);
const NIL_SESSION: &str = "00000000-0000-0000-0000-000000000000";

/// A runner's registration, read from its `.runner`, `.credentials`, and
/// `.credentials_rsaparams` files.
pub struct Identity {
    pub agent_id: u64,
    pub agent_name: String,
    pub broker_url: String,
    pub disable_update: bool,
    client_id: String,
    audience: String,
    token_url: String,
    key: RsaKeyPair,
}

fn field<'a>(value: &'a Value, name: &str, file: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .filter(|text| !text.is_empty())
        .context(format!("{file} has no {name}"))
}

fn der_length(length: usize, out: &mut Vec<u8>) {
    if length < 0x80 {
        out.push(length as u8);
    } else {
        let bytes: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
}

fn der_integer(unsigned: &[u8], out: &mut Vec<u8>) {
    let trimmed: Vec<u8> = unsigned.iter().copied().skip_while(|byte| *byte == 0).collect();
    let mut content = if trimmed.is_empty() { vec![0] } else { trimmed };
    // DER integers are signed: keep a set high bit from reading as negative.
    if content[0] & 0x80 != 0 {
        content.insert(0, 0);
    }
    out.push(0x02);
    der_length(content.len(), out);
    out.extend(content);
}

/// A PKCS#1 RSAPrivateKey in DER, from the components .NET serializes.
pub fn pkcs1_der(params: &Value) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    der_integer(&[0], &mut body);
    for name in ["modulus", "exponent", "d", "p", "q", "dp", "dq", "inverseQ"] {
        let encoded = field(params, name, ".credentials_rsaparams")?;
        let bytes = STANDARD
            .decode(encoded)
            .context(format!(".credentials_rsaparams {name}"))?;
        der_integer(&bytes, &mut body);
    }
    let mut der = vec![0x30];
    der_length(body.len(), &mut der);
    der.extend(body);
    Ok(der)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Identity {
    pub fn load(runner_dir: &Path) -> Result<Self> {
        let read = |name: &str| -> Result<Value> {
            read_record(&runner_dir.join(name))?
                .context(format!("{}/{name} is missing", runner_dir.display()))
        };
        let settings = read(".runner")?;
        if settings["useV2Flow"].as_bool() != Some(true) {
            bail!("This runner uses GitHub's older message flow; only the broker flow is supported");
        }
        let mut broker_url = field(&settings, "serverUrlV2", ".runner")?.to_owned();
        if !broker_url.ends_with('/') {
            broker_url.push('/');
        }
        let credentials = read(".credentials")?;
        if credentials["scheme"].as_str() != Some("OAuth") {
            bail!(".credentials does not hold OAuth runner credentials");
        }
        let data = &credentials["data"];
        let audience = field(data, "authorizationUrl", ".credentials")?.to_owned();
        let token_url = data["oauthEndpointUrl"].as_str().unwrap_or(&audience).to_owned();
        let der = pkcs1_der(&read(".credentials_rsaparams")?)?;
        let key = RsaKeyPair::from_der(&der).map_err(|error| Error(format!("runner RSA key: {error}")))?;
        Ok(Identity {
            agent_id: settings["agentId"].as_u64().context(".runner has no agentId")?,
            agent_name: field(&settings, "agentName", ".runner")?.to_owned(),
            broker_url,
            disable_update: settings["disableUpdate"].as_bool().unwrap_or(false),
            client_id: field(data, "clientId", ".credentials")?.to_owned(),
            audience,
            token_url,
            key,
        })
    }

    /// The client assertion: a five-minute RS256 JWT, as the runner signs it.
    pub fn assertion(&self, now: u64) -> Result<String> {
        let id = system::random_hex(16)?;
        let header = json!({"alg": "RS256", "typ": "JWT"});
        let claims = json!({
            "iss": self.client_id, "sub": self.client_id, "aud": self.audience,
            "nbf": now, "iat": now, "exp": now + 300,
            "jti": format!("{}-{}-{}-{}-{}", &id[..8], &id[8..12], &id[12..16], &id[16..20], &id[20..]),
        });
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| Error("signing the runner's JWT failed".into()))?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }
}

/// A message the broker delivered: its ID and type, never its contents.
#[derive(Debug)]
pub struct Message {
    pub id: i64,
    pub kind: String,
}

pub struct Broker {
    identity: Identity,
    runner_version: String,
    arch: Arch,
    agent: ureq::Agent,
    token: String,
}

fn body_text(response: ureq::Response) -> String {
    response.into_string().unwrap_or_default()
}

impl Broker {
    pub fn connect(identity: Identity, runner_version: String) -> Result<Self> {
        // The runner's own platform, as it would announce itself.
        let arch = Arch::host()?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(LONG_POLL)
            .user_agent(&format!(
                "GitHubActionsRunner-linux-{}/{runner_version} runberth",
                arch.runner()
            ))
            .build();
        let assertion = identity.assertion(unix_now())?;
        let reply = agent.post(&identity.token_url).send_form(&[
            ("grant_type", "client_credentials"),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ),
            ("client_assertion", &assertion),
        ]);
        let token: Value = match reply {
            Ok(response) => serde_json::from_str(&body_text(response))?,
            Err(ureq::Error::Status(code, response)) => {
                bail!("Runner token request failed ({code}): {}", body_text(response))
            }
            Err(error) => bail!("Runner token request: {error}"),
        };
        let token = field(&token, "access_token", "token response")?.to_owned();
        Ok(Broker {
            identity,
            runner_version,
            arch,
            agent,
            token,
        })
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{}{path}", self.identity.broker_url))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
    }

    pub fn create_session(&self) -> Result<String> {
        let session = json!({
            "sessionId": NIL_SESSION,
            "ownerName": format!("runberth (PID: {})", std::process::id()),
            "agent": {
                "id": self.identity.agent_id,
                "name": self.identity.agent_name,
                "version": self.runner_version,
                "osDescription": "Linux",
            },
            "useFipsEncryption": false,
        });
        let reply = self
            .request("POST", "session")
            .set("Content-Type", "application/json")
            .send_string(&session.to_string());
        let created: Value = match reply {
            Ok(response) => serde_json::from_str(&body_text(response))?,
            Err(ureq::Error::Status(409, _)) => {
                bail!("A session for this runner already exists; is its service still running?")
            }
            Err(ureq::Error::Status(code, response)) => {
                bail!(
                    "Creating a broker session failed ({code}): {}",
                    body_text(response)
                )
            }
            Err(error) => bail!("Creating a broker session: {error}"),
        };
        Ok(field(&created, "sessionId", "session response")?.to_owned())
    }

    /// Wait for the next message; None when the long poll ends without one.
    pub fn next_message(&self, session: &str) -> Result<Option<Message>> {
        let reply = self
            .request("GET", "message")
            .query("sessionId", session)
            .query("status", "Online")
            .query("runnerVersion", &self.runner_version)
            .query("os", "Linux")
            .query("architecture", self.arch.broker())
            .query(
                "disableUpdate",
                if self.identity.disable_update {
                    "true"
                } else {
                    "false"
                },
            )
            .call();
        let text = match reply {
            Ok(response) => body_text(response),
            Err(ureq::Error::Status(code, response)) => {
                bail!(
                    "Waiting for a broker message failed ({code}): {}",
                    body_text(response)
                )
            }
            Err(ureq::Error::Transport(error)) if error.kind() == ureq::ErrorKind::Io => return Ok(None),
            Err(error) => bail!("Waiting for a broker message: {error}"),
        };
        if text.trim().is_empty() || text.trim() == "null" {
            return Ok(None);
        }
        let message: Value = serde_json::from_str(&text)?;
        Ok(Some(Message {
            id: message["messageId"].as_i64().unwrap_or_default(),
            kind: message["messageType"].as_str().unwrap_or("unknown").to_owned(),
        }))
    }

    pub fn delete_session(&self, session: &str) -> Result<()> {
        match self
            .request("DELETE", "session")
            .query("sessionId", session)
            .call()
        {
            Ok(_) | Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(ureq::Error::Status(code, response)) => {
                bail!(
                    "Deleting the broker session failed ({code}): {}",
                    body_text(response)
                )
            }
            Err(error) => bail!("Deleting the broker session: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};

    use super::*;

    fn params() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-only-rsa-params.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn identity() -> Identity {
        Identity {
            agent_id: 7,
            agent_name: "runberth-repo".into(),
            broker_url: "https://broker.example/".into(),
            disable_update: false,
            client_id: "client-123".into(),
            audience: "https://auth.example/token".into(),
            token_url: "https://auth.example/token".into(),
            key: RsaKeyPair::from_der(&pkcs1_der(&params()).unwrap()).unwrap(),
        }
    }

    #[test]
    fn der_integers_are_minimal_and_positive() {
        let mut out = Vec::new();
        der_integer(&[0, 0, 0x7f], &mut out);
        der_integer(&[0x80], &mut out);
        der_integer(&[], &mut out);
        assert_eq!(out, [0x02, 1, 0x7f, 0x02, 2, 0, 0x80, 0x02, 1, 0]);
        let mut long = Vec::new();
        der_length(300, &mut long);
        assert_eq!(long, [0x82, 0x01, 0x2c]);
    }

    #[test]
    fn assertion_is_a_verifiable_rs256_jwt_for_the_runner() {
        let jwt = identity().assertion(1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "client-123");
        assert_eq!(claims["sub"], "client-123");
        assert_eq!(claims["aud"], "https://auth.example/token");
        assert_eq!(claims["exp"].as_u64(), Some(1_700_000_300));
        assert_eq!(claims["jti"].as_str().unwrap().len(), 36);

        let p = params();
        let decode = |name: &str| STANDARD.decode(p[name].as_str().unwrap()).unwrap();
        let public = RsaPublicKeyComponents {
            n: decode("modulus"),
            e: decode("exponent"),
        };
        let signed = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        public
            .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn identity_requires_the_broker_flow() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".runner"), r#"{"agentId": 7, "agentName": "x"}"#).unwrap();
        let error = Identity::load(temp.path()).err().unwrap();
        assert!(error.0.contains("older message flow"), "{error}");
    }
}
