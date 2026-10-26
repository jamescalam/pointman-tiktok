//! TikTok's HTTP APIs: OAuth v2 (a code or a refresh token into tokens) and the Content Posting API, with an access
//! token that's made again when it runs out. Errors say what TikTok said, never a token.

use std::io::Read;
use std::time::Duration;

use anyhow::anyhow;
use regex::Regex;
use serde_json::Value;

/// Where the calls go: TikTok's own, unless a test's stand-in is named in the environment (the node passes on only
/// PATH, HOME and the locale, so only a test can).
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// OAuth and the Content Posting API: `{api}/v2/oauth/token/`, `{api}/v2/post/publish/…`.
    pub api: String,
}

impl Endpoints {
    pub fn from_env() -> Self {
        let api = std::env::var("TIKTOK_API_BASE").ok().filter(|v| !v.is_empty());
        Endpoints { api: api.unwrap_or_else(|| "https://open.tiktokapis.com".into()).trim_end_matches('/').to_string() }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/v2/{path}", self.api)
    }
}

/// Where a person signs in: Login Kit's authorize page (https://developers.tiktok.com/doc/login-kit-desktop).
pub const AUTHORIZE: &str = "https://www.tiktok.com/v2/auth/authorize/";

/// The scope a draft needs (the inbox upload).
pub const SCOPE_UPLOAD: &str = "video.upload";
/// The scope a direct post and the creator's info need.
pub const SCOPE_PUBLISH: &str = "video.publish";
/// Login Kit's own, which every app has.
pub const SCOPE_BASIC: &str = "user.info.basic";

/// The TikTok app the sign-ins and uploads go through (the vault's `tiktok.client`): its client key and secret.
#[derive(Clone)]
pub struct Client {
    pub key: String,
    pub secret: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("key", &self.key).finish_non_exhaustive()
    }
}

impl Client {
    /// The vault item's value: the client key, then its secret (a space or a colon between).
    pub fn parse(value: &str) -> Option<Self> {
        let re = Regex::new(r"^\s*([A-Za-z0-9]{8,64})[\s:]+([A-Za-z0-9_-]{16,128})\s*$").expect("compiles");
        let m = re.captures(value)?;
        Some(Client { key: m[1].to_string(), secret: m[2].to_string() })
    }
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(300))
        .timeout_write(Duration::from_secs(300))
        .user_agent(concat!("pointman-tiktok/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// What went wrong with a call.
#[derive(Debug)]
pub enum Failure {
    /// TikTok answered with an error: its status, its code (`access_token_invalid`, `invalid_grant`) and its words.
    Said { status: u16, code: String, message: String },
    /// No answer: the connection dropped or timed out.
    Lost(String),
}

impl Failure {
    pub fn code(&self) -> &str {
        match self {
            Failure::Said { code, .. } => code,
            Failure::Lost(_) => "",
        }
    }
}

/// What a TikTok error code means for the person, where its own words don't say.
fn hint(code: &str) -> &'static str {
    match code {
        "spam_risk_too_many_pending_share" => {
            " (5 drafts are waiting in the account's TikTok inbox already, the most TikTok allows in 24 hours: post or \
             delete some in the TikTok app first)"
        }
        "unaudited_client_can_only_post_to_private_accounts" => {
            " (TikTok hasn't audited this app yet: until it does, a direct post needs the account set to private in \
             the TikTok app, and goes up as self_only. Or send it as a draft)"
        }
        "spam_risk_too_many_posts" => " (the account has posted as much as TikTok allows through apps today)",
        "reached_active_user_cap" => " (the TikTok app has reached its daily number of posting accounts)",
        "scope_not_authorized" => {
            " (the sign-in didn't allow this: sign in again with /tiktok add, and allow posting; a direct post needs \
             the direct_post setting on)"
        }
        "privacy_level_option_mismatch" => " (that privacy isn't one of the account's options now: see tiktok.creator)",
        "rate_limit_exceeded" => " (too many calls this minute: wait a minute)",
        "spam_risk_user_banned_from_posting" => " (TikTok has stopped this account posting)",
        _ => "",
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Said { status, code, message } => {
                write!(f, "TikTok said {status}")?;
                if !code.is_empty() {
                    write!(f, " {code}")?;
                }
                if !message.is_empty() {
                    write!(f, ": {message}")?;
                }
                f.write_str(hint(code))
            }
            Failure::Lost(why) => write!(f, "no answer from TikTok: {why}"),
        }
    }
}

impl std::error::Error for Failure {}

/// TikTok's error in a body, if it has one: the API's `{error: {code, message, log_id}}` (where `ok` is no error),
/// or OAuth's `{error, error_description, log_id}`. TikTok sends some of these with a 200.
pub fn error_in(status: u16, v: &Value) -> Option<Failure> {
    match &v["error"] {
        Value::Object(e) => {
            let code = e.get("code").and_then(Value::as_str).unwrap_or_default();
            (code != "ok" && !code.is_empty()).then(|| Failure::Said {
                status,
                code: code.to_string(),
                message: e.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
            })
        }
        Value::String(code) if !code.is_empty() => Some(Failure::Said {
            status,
            code: code.clone(),
            message: v["error_description"].as_str().unwrap_or_default().to_string(),
        }),
        _ => None,
    }
}

/// What a failed answer's body said.
pub fn said(status: u16, body: &str) -> Failure {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    error_in(status, &v).unwrap_or_else(|| Failure::Said {
        status,
        code: String::new(),
        message: body.chars().take(200).collect(),
    })
}

/// A response's body as text, at most 4 MiB.
pub fn body(r: ureq::Response) -> String {
    let mut s = String::new();
    let _ = r.into_reader().take(4 << 20).read_to_string(&mut s);
    s
}

/// What went wrong with a request.
pub fn failure(e: ureq::Error) -> Failure {
    match e {
        ureq::Error::Status(status, r) => said(status, &body(r)),
        ureq::Error::Transport(t) => Failure::Lost(t.to_string()),
    }
}

/// The answer to a request, any status below 400 as Ok, or what went wrong.
pub fn send(r: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, Failure> {
    r.map_err(failure)
}

/// An answer's JSON, or the error in it.
pub fn json(r: ureq::Response) -> Result<Value, Failure> {
    let status = r.status();
    let text = body(r);
    let v: Value = serde_json::from_str(&text).map_err(|_| Failure::Said {
        status,
        code: String::new(),
        message: format!("not JSON: {}", text.chars().take(200).collect::<String>()),
    })?;
    match error_in(status, &v) {
        Some(f) => Err(f),
        None => Ok(v),
    }
}

/// What the token endpoint gives.
pub struct Tokens {
    pub access: String,
    pub lasts: Duration,
    pub refresh: String,
    /// The scopes the person allowed, as TikTok lists them.
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").field("lasts", &self.lasts).field("scopes", &self.scopes).finish_non_exhaustive()
    }
}

/// The token endpoint, with a grant's own fields
/// (https://developers.tiktok.com/doc/oauth-user-access-token-management).
pub fn token(agent: &ureq::Agent, ends: &Endpoints, client: &Client, grant: &[(&str, &str)]) -> anyhow::Result<Tokens> {
    let mut form = vec![("client_key", client.key.as_str()), ("client_secret", client.secret.as_str())];
    form.extend_from_slice(grant);
    let got =
        send(agent.post(&ends.url("oauth/token/")).set("Cache-Control", "no-cache").send_form(&form)).and_then(json);
    let v = match got {
        Ok(v) => v,
        Err(Failure::Said { code, .. }) if code == "invalid_client" || code == "unauthorized_client" => {
            return Err(anyhow!(
                "TikTok refused the app ({code}): check tiktok.client is its client key and secret, from TikTok for \
                 Developers › Manage apps, and that the app has Login Kit"
            ))
        }
        Err(e) => return Err(e.into()),
    };
    let text = |k: &str| v[k].as_str().filter(|t| !t.is_empty()).map(String::from);
    let access = text("access_token").ok_or_else(|| anyhow!("TikTok gave no access token"))?;
    let refresh = text("refresh_token").ok_or_else(|| anyhow!("TikTok gave no refresh token"))?;
    let scopes = v["scope"].as_str().unwrap_or_default().split([',', ' ']).filter(|s| !s.is_empty()).map(String::from);
    Ok(Tokens {
        access,
        lasts: Duration::from_secs(v["expires_in"].as_u64().unwrap_or(86_400)),
        refresh,
        scopes: scopes.collect(),
    })
}

/// A refresh token into a new access token (and maybe a new refresh token: TikTok can rotate it).
pub fn refresh(agent: &ureq::Agent, ends: &Endpoints, client: &Client, refresh: &str) -> anyhow::Result<Tokens> {
    token(agent, ends, client, &[("grant_type", "refresh_token"), ("refresh_token", refresh)]).map_err(|e| {
        match e.downcast_ref::<Failure>().map(Failure::code) {
            Some("invalid_grant") => anyhow!(
                "TikTok no longer takes this sign-in (invalid_grant: revoked, a year old, or replaced by a newer \
                 one): sign the account in again with /tiktok add"
            ),
            _ => e,
        }
    })
}

/// Calls to the Content Posting API with a token from `token` (true: make a new one, the last was refused), tried
/// once more with a new token when TikTok says the one it had is invalid.
pub struct Api<'a> {
    pub agent: &'a ureq::Agent,
    pub ends: &'a Endpoints,
    pub token: &'a (dyn Fn(bool) -> anyhow::Result<String> + Sync),
}

impl Api<'_> {
    /// A POST of `body` to `{api}/v2/{path}`: the answer's `data`.
    pub fn post(&self, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let url = self.ends.url(path);
        let once = |fresh: bool| -> anyhow::Result<Result<Value, Failure>> {
            let auth = format!("Bearer {}", (self.token)(fresh)?);
            // TikTok asks for the JSON type even on a call with no body (creator_info/query)
            let req = self
                .agent
                .post(&url)
                .set("Authorization", &auth)
                .set("Content-Type", "application/json; charset=UTF-8");
            let got = match body {
                Some(b) => req.send_string(&b.to_string()),
                None => req.call(),
            };
            Ok(send(got).and_then(json))
        };
        let got = match once(false)? {
            Err(Failure::Said { status: 401, code, .. }) if code == "access_token_invalid" => once(true)?,
            got => got,
        };
        Ok(got?["data"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_client_is_its_key_and_secret() {
        let c = Client::parse("awfakeclientkey01 fakeclientsecret0123456789abcdef").unwrap();
        assert_eq!(c.key, "awfakeclientkey01");
        assert_eq!(c.secret, "fakeclientsecret0123456789abcdef");
        assert!(Client::parse("sbawkey12345:secret-secret-secret_1").is_some());
        assert!(Client::parse("fakeclientsecret0123456789abcdef").is_none());
        assert!(!format!("{c:?}").contains("fakeclientsecret"), "its Debug leaves the secret out");
    }

    #[test]
    fn tiktoks_errors_say_their_code_even_with_a_200() {
        let ok =
            json!({"data": {"publish_id": "v_inbox_file~v2.1"}, "error": {"code": "ok", "message": "", "log_id": "x"}});
        assert!(error_in(200, &ok).is_none());
        let pending =
            json!({"error": {"code": "spam_risk_too_many_pending_share", "message": "Too many", "log_id": "x"}});
        let e = error_in(403, &pending).unwrap().to_string();
        assert!(e.contains("spam_risk_too_many_pending_share") && e.contains("5 drafts"), "{e}");
        let oauth = json!({"error": "invalid_grant", "error_description": "Refresh token is invalid", "log_id": "x"});
        assert_eq!(error_in(200, &oauth).unwrap().code(), "invalid_grant");
        let e = said(403, r#"{"error": {"code": "unaudited_client_can_only_post_to_private_accounts"}}"#).to_string();
        assert!(e.contains("set to private") && e.contains("draft"), "{e}");
    }
}
