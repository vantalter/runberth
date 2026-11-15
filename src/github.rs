//! A minimal GitHub REST client. The `Api` trait lets tests substitute
//! canned responses for the network.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bail;
use crate::error::{Context, Error, Result};
use crate::names::hex;
use crate::system::signals;

pub const API: &str = "https://api.github.com";
const API_VERSION: &str = "2026-03-10";

pub struct Reply {
    pub status: u16,
    pub body: Value,
}

pub trait Api {
    fn call(&self, method: &str, url: &str) -> Result<Reply>;

    /// A successful (2xx) response body, or an error naming the request.
    fn json(&self, method: &str, url: &str) -> Result<Value> {
        let reply = self.call(method, url)?;
        if !(200..300).contains(&reply.status) {
            bail!("GitHub returned {} for {method} {url}", reply.status);
        }
        Ok(reply.body)
    }
}

pub struct GitHub {
    token: String,
    agent: ureq::Agent,
}

impl GitHub {
    pub fn new(token: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("runberth/", env!("CARGO_PKG_VERSION")))
            .build();
        GitHub { token, agent }
    }
}

impl Api for GitHub {
    fn call(&self, method: &str, url: &str) -> Result<Reply> {
        let request = self
            .agent
            .request(method, url)
            .set("Accept", "application/vnd.github+json")
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("X-GitHub-Api-Version", API_VERSION);
        let result = if method == "POST" {
            request.send_string("")
        } else {
            request.call()
        };
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => bail!("{method} {url}: {error}"),
        };
        let status = response.status();
        let text = response.into_string().context(url)?;
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).context(url)?
        };
        Ok(Reply { status, body })
    }
}

/// Download a public file, failing unless its SHA-256 matches.
pub fn download(url: &str, sha256: &str, destination: &Path) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(60))
        .build();
    let response = agent
        .get(url)
        .call()
        .map_err(|error| Error(format!("GET {url}: {error}")))?;
    let mut reader = response.into_reader();
    let mut output = File::create(destination).context(destination.display())?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        if signals::requested() {
            bail!("Interrupted");
        }
        let read = reader.read(&mut buffer).context(url)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        output.write_all(&buffer[..read]).context(destination.display())?;
    }
    if hex(&digest.finalize()) != sha256 {
        let _ = std::fs::remove_file(destination);
        bail!("Downloaded file's checksum does not match the published SHA-256: {url}");
    }
    Ok(())
}

#[cfg(test)]
pub mod testing {
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};

    use super::*;

    /// Canned responses keyed by "METHOD URL", served in order.
    #[derive(Default)]
    pub struct Fake {
        replies: RefCell<HashMap<String, VecDeque<Reply>>>,
        pub calls: RefCell<Vec<String>>,
    }

    impl Fake {
        pub fn reply(self, method: &str, url: &str, status: u16, body: Value) -> Self {
            self.replies
                .borrow_mut()
                .entry(format!("{method} {url}"))
                .or_default()
                .push_back(Reply { status, body });
            self
        }
    }

    impl Api for Fake {
        fn call(&self, method: &str, url: &str) -> Result<Reply> {
            let key = format!("{method} {url}");
            self.calls.borrow_mut().push(key.clone());
            match self
                .replies
                .borrow_mut()
                .get_mut(&key)
                .and_then(VecDeque::pop_front)
            {
                Some(reply) => Ok(reply),
                None => bail!("unexpected request {key}"),
            }
        }
    }
}
