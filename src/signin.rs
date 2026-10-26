//! Signing a TikTok account in from the apps with Login Kit for Desktop
//! (https://developers.tiktok.com/doc/login-kit-desktop): an authorization code with PKCE, sent to a fixed local
//! address. TikTok has no device flow, so the address is never served: the person signs in on any device, the page
//! after won't load, and its address holds the `code` and the `state`. They paste that whole address into the app's
//! form, and `credentials.code` checks its state is this sign-in's, then trades the code for the tokens.
//!
//! The value that goes in the vault is the refresh token, alone. Nothing here writes it anywhere else, logs it or
//! keeps it once it's given.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use pointman_extension::protocol::CredentialsSignInResponse;
use sha2::{Digest, Sha256};

use crate::tiktok::{self, Client, Endpoints, AUTHORIZE, SCOPE_BASIC, SCOPE_PUBLISH, SCOPE_UPLOAD};

/// How long a sign-in waits for its address to be pasted.
pub const WAIT: Duration = Duration::from_secs(15 * 60);

/// What the form tells the person (at most 200 characters, the node's limit).
pub const PROMPT: &str = "Sign in to TikTok and allow it. The page after won't open: copy its whole address \
                          (http://127.0.0.1…) and paste it here.";

struct Pending {
    client: Client,
    verifier: String,
    state: String,
    redirect: String,
    scopes: Vec<&'static str>,
    until: Instant,
}

/// The sign-ins under way, by the node's session id.
#[derive(Default)]
pub struct SignIns {
    open: Mutex<HashMap<String, Pending>>,
}

/// `n` random characters from PKCE's unreserved set (A-Z a-z 0-9 - _: 64 of them, so each byte's low 6 bits pick
/// one evenly).
pub fn random(n: usize) -> anyhow::Result<String> {
    const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = vec![0u8; n];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow!("no randomness for the sign-in: {e}"))?;
    Ok(bytes.iter().map(|b| SET[(b & 63) as usize] as char).collect())
}

/// The code challenge for a verifier, as TikTok's desktop flow takes it: the SHA-256 in hex (not RFC 7636's base64).
pub fn challenge(verifier: &str) -> String {
    Sha256::digest(verifier.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// What a pasted redirect address says.
#[derive(Debug, Default, PartialEq)]
pub struct Pasted {
    pub code: String,
    pub state: String,
    pub error: String,
    pub error_description: String,
}

/// The query of what was pasted: the whole address, or only its `?…` part.
pub fn read_paste(text: &str) -> Pasted {
    let text = text.trim().trim_matches(['"', '\'', '<', '>']);
    let query = match text.split_once('?') {
        Some((_, q)) => q,
        None => text,
    };
    let query = query.split('#').next().unwrap_or_default();
    let mut out = Pasted::default();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        let v = v.trim().to_string();
        match k.as_ref() {
            "code" => out.code = v,
            "state" => out.state = v,
            "error" => out.error = v,
            "error_description" => out.error_description = v,
            _ => {}
        }
    }
    out
}

impl SignIns {
    fn take(&self, session: &str) -> anyhow::Result<Pending> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|_, p| p.until > Instant::now());
        open.remove(session).ok_or_else(|| anyhow!("this sign-in has ended: start it again"))
    }

    /// Start one: TikTok's authorize link, for the person to open on any device.
    pub fn start(
        &self,
        client: Client,
        redirect: &str,
        direct_post: bool,
        session: &str,
    ) -> anyhow::Result<CredentialsSignInResponse> {
        if session.is_empty() {
            bail!("a sign-in needs the node's session");
        }
        let mut scopes = vec![SCOPE_BASIC, SCOPE_UPLOAD];
        if direct_post {
            scopes.push(SCOPE_PUBLISH);
        }
        let verifier = random(64)?;
        let state = random(32)?;
        let mut url = url::Url::parse(AUTHORIZE).expect("parses");
        url.query_pairs_mut()
            .append_pair("client_key", &client.key)
            .append_pair("response_type", "code")
            .append_pair("scope", &scopes.join(","))
            .append_pair("redirect_uri", redirect)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        self.open.lock().unwrap_or_else(|e| e.into_inner()).insert(
            session.to_string(),
            Pending { client, verifier, state, redirect: redirect.to_string(), scopes, until: Instant::now() + WAIT },
        );
        Ok(CredentialsSignInResponse { url: url.to_string(), prompt: Some(PROMPT.into()), ..Default::default() })
    }

    /// The person pasted the address the sign-in ended on: the refresh token, once its state is this sign-in's and
    /// TikTok has traded the code.
    pub fn code(&self, agent: &ureq::Agent, ends: &Endpoints, session: &str, text: &str) -> anyhow::Result<String> {
        let p = self.take(session)?;
        let got = read_paste(text);
        if !got.error.is_empty() {
            let why =
                if got.error_description.is_empty() { String::new() } else { format!(": {}", got.error_description) };
            bail!("TikTok didn't sign it in ({}{why}): start again", got.error);
        }
        if got.code.is_empty() || got.state.is_empty() {
            // nothing of this sign-in was given: it carries on, for the right paste
            self.open.lock().unwrap_or_else(|e| e.into_inner()).insert(session.to_string(), p);
            bail!(
                "that isn't the address the sign-in ended on: paste all of it, from http:// to the end, with its \
                 code= and state="
            );
        }
        if got.state != p.state {
            bail!("that address is from another sign-in (its state isn't this one's), so it's refused: start again");
        }
        let tokens = tiktok::token(
            agent,
            ends,
            &p.client,
            &[
                ("code", got.code.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", p.redirect.as_str()),
                ("code_verifier", p.verifier.as_str()),
            ],
        )?;
        if !tokens.scopes.iter().any(|s| s == SCOPE_UPLOAD) {
            bail!("the sign-in didn't allow uploading videos ({SCOPE_UPLOAD}): sign in again and allow it");
        }
        let missing: Vec<&str> = p.scopes.iter().copied().filter(|s| !tokens.scopes.iter().any(|t| t == s)).collect();
        if missing.contains(&SCOPE_PUBLISH) {
            bail!(
                "the sign-in didn't allow direct posts ({SCOPE_PUBLISH}): sign in again and allow it, or turn the \
                 direct_post setting off for drafts only"
            );
        }
        Ok(tokens.refresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_verifiers_sha256_in_hex() {
        // sha256("abc"), as every SHA-256 test suite has it
        assert_eq!(challenge("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let v = random(64).unwrap();
        assert_eq!(v.len(), 64);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{v}");
        assert_ne!(v, random(64).unwrap());
    }

    #[test]
    fn a_paste_is_read_as_the_whole_address_or_its_query() {
        let whole = "http://127.0.0.1:8370/callback/?code=abc%2A123%21&scopes=user.info.basic%2Cvideo.upload&state=s1";
        assert_eq!(read_paste(whole), Pasted { code: "abc*123!".into(), state: "s1".into(), ..Default::default() });
        assert_eq!(read_paste(&format!("  \"{whole}#_\" ")).code, "abc*123!");
        assert_eq!(read_paste("code=x&state=y").state, "y");
        let no = read_paste("http://127.0.0.1:8370/callback/?error=access_denied&error_description=User+cancelled");
        assert_eq!((no.error.as_str(), no.error_description.as_str()), ("access_denied", "User cancelled"));
        assert_eq!(read_paste("WDJB-MJHT"), Pasted::default());
        assert!(PROMPT.chars().count() <= 200);
    }
}
