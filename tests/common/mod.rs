//! A fake TikTok: OAuth v2's token endpoint (an authorization code with PKCE, refresh tokens it may rotate) and the
//! parts of the Content Posting API the extension calls, on a local port, over plain HTTP/1.1. It keeps every request
//! it was sent, and behaves as TikTok's docs say where it matters to the tests: a code is traded only with the
//! verifier whose hex SHA-256 was the challenge, an init's chunks follow the media transfer guide's rules, chunks go
//! up in order (a 206 until the last, then 201), a direct post's privacy must be one the creator has, and a request
//! without a token it gave is a 401 `access_token_invalid`. Errors are TikTok's shape, some with a 200.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const CLIENT_KEY: &str = "awfakeclientkey01";
pub const CLIENT_SECRET: &str = "fakeclientsecret0123456789abcdef";
pub const REFRESH: &str = "rft.fakeREFRESHtokenFORtheTESTS0001";
pub const REFRESH_2: &str = "rft.fakeREFRESHtokenROTATEDtests0002";
pub const CODE: &str = "fake-code*123!";
pub const REDIRECT: &str = "http://127.0.0.1:8370/callback/";
pub const MIB: usize = 1024 * 1024;

/// One request it was sent.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    /// How many bytes of its body it read (all of them, but for a dropped one).
    pub read: usize,
    pub json: Value,
    pub form: HashMap<String, String>,
}

pub struct Upload {
    pub publish_id: String,
    pub direct: bool,
    pub size: usize,
    pub chunk_size: usize,
    pub count: usize,
    pub data: Vec<u8>,
    pub post_info: Value,
    pub status: String,
    pub fail_reason: String,
    pub post_ids: Vec<u64>,
}

pub struct State {
    pub requests: Vec<Req>,
    /// The challenge the sign-in sent, which the token endpoint checks the verifier against.
    pub challenge: Option<String>,
    /// Refresh tokens it takes, and how long the access tokens it gives last.
    pub refresh_tokens: Vec<String>,
    pub expires_in: u64,
    /// Give a new refresh token on a refresh, and stop taking the old one.
    pub rotate: bool,
    /// The scopes a sign-in grants.
    pub scopes: String,
    pub tokens: Vec<String>,
    /// Tokens it says 401 to once, as if they had run out.
    pub expire: Vec<String>,
    /// The creator's info, as creator_info/query gives it.
    pub creator: Value,
    /// TikTok hasn't audited the app: a direct post needs a private account (no PUBLIC_TO_EVERYONE option) and
    /// SELF_ONLY.
    pub unaudited: bool,
    /// Uploads by their number in the upload address.
    pub uploads: Vec<Upload>,
    /// Drop the connection once, partway through the chunk that reaches this byte of an upload.
    pub drop_at: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        State {
            requests: vec![],
            challenge: None,
            refresh_tokens: vec![REFRESH.into()],
            expires_in: 86_400,
            rotate: false,
            scopes: "user.info.basic,video.upload,video.publish".into(),
            tokens: vec![],
            expire: vec![],
            creator: json!({
                "creator_avatar_url": "https://p16.tiktokcdn.com/fake.jpeg",
                "creator_username": "jamescalam",
                "creator_nickname": "James Briggs",
                "privacy_level_options": ["PUBLIC_TO_EVERYONE", "MUTUAL_FOLLOW_FRIENDS", "SELF_ONLY"],
                "comment_disabled": false,
                "duet_disabled": false,
                "stitch_disabled": true,
                "max_video_post_duration_sec": 600
            }),
            unaudited: false,
            uploads: vec![],
            drop_at: None,
        }
    }
}

#[derive(Clone)]
pub struct Fake {
    pub url: String,
    state: Arc<Mutex<State>>,
}

fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl Fake {
    pub fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let fake = Fake { url, state: Arc::new(Mutex::new(State::default())) };
        let f = fake.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let f = f.clone();
                std::thread::spawn(move || f.serve(stream));
            }
        });
        fake
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn requests(&self) -> Vec<Req> {
        self.state().requests.clone()
    }

    pub fn paths(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.path).collect()
    }

    /// The requests to the Content Posting API and the uploads (not OAuth's).
    pub fn api_requests(&self) -> Vec<Req> {
        self.requests().into_iter().filter(|r| r.path != "/v2/oauth/token/").collect()
    }

    fn serve(&self, mut stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut words = line.split_whitespace();
        let method = words.next().unwrap_or_default().to_string();
        let target = words.next().unwrap_or_default().to_string();
        let mut headers = HashMap::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), form(q)),
            None => (target.clone(), HashMap::new()),
        };
        let length: usize = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);

        // a chunk whose connection drops: read part of it, keep none of it (TikTok takes whole chunks), hang up
        if method == "PUT" && path == "/upload/" {
            if let Some((from, _, _)) = content_range(&headers) {
                let drop = self.state().drop_at.filter(|at| (from..from + length).contains(at));
                if let Some(at) = drop {
                    let mut part = vec![0u8; at - from];
                    let read = reader.read_exact(&mut part).map(|_| part.len()).unwrap_or(0);
                    let mut s = self.state();
                    s.drop_at = None;
                    s.requests.push(Req {
                        method,
                        path,
                        query,
                        headers,
                        read,
                        json: Value::Null,
                        form: HashMap::new(),
                    });
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
            }
        }
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let as_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let as_form = if path == "/v2/oauth/token/" { form(&String::from_utf8_lossy(&body)) } else { HashMap::new() };
        self.state().requests.push(Req {
            method: method.clone(),
            path: path.clone(),
            query: query.clone(),
            headers: headers.clone(),
            read: body.len(),
            json: as_json.clone(),
            form: as_form.clone(),
        });
        let (status, extra, out) = if path == "/v2/oauth/token/" {
            self.oauth(&as_form)
        } else {
            self.answer(&method, &path, &query, &headers, &body, &as_json)
        };
        let text = if out.is_null() { String::new() } else { out.to_string() };
        let mut head = format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n",
            reason(status),
            text.len()
        );
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(text.as_bytes());
        let _ = stream.flush();
    }

    fn authorized(&self, headers: &HashMap<String, String>) -> bool {
        let Some(token) = headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ")) else { return false };
        let mut s = self.state();
        if let Some(i) = s.expire.iter().position(|t| t == token) {
            s.expire.remove(i);
            return false;
        }
        s.tokens.iter().any(|t| t == token)
    }

    fn answer(
        &self,
        method: &str,
        path: &str,
        query: &HashMap<String, String>,
        headers: &HashMap<String, String>,
        body: &[u8],
        body_json: &Value,
    ) -> (u16, Vec<(String, String)>, Value) {
        let error = |status: u16, code: &str, message: &str| {
            (status, vec![], json!({"error": {"code": code, "message": message, "log_id": "fake-log"}}))
        };
        let ok = |data: Value| {
            (200, vec![], json!({"data": data, "error": {"code": "ok", "message": "", "log_id": "fake-log"}}))
        };

        // the upload address needs no token: it's the address itself
        if method == "PUT" && path == "/upload/" {
            return self.chunk(query, headers, body);
        }
        if method != "POST" {
            return error(404, "not_found", "no such endpoint in the fake");
        }
        if !self.authorized(headers) {
            return error(401, "access_token_invalid", "The access token is invalid or not found in the request.");
        }
        let mut s = self.state();
        match path {
            "/v2/post/publish/creator_info/query/" => {
                let c = s.creator.clone();
                ok(c)
            }
            "/v2/post/publish/inbox/video/init/" | "/v2/post/publish/video/init/" => {
                let direct = path == "/v2/post/publish/video/init/";
                if !direct && body_json.get("post_info").is_some() {
                    return error(400, "invalid_param", "the inbox init takes no post_info");
                }
                let src = &body_json["source_info"];
                if src["source"] != "FILE_UPLOAD" {
                    return error(400, "invalid_param", "source");
                }
                let size = src["video_size"].as_u64().unwrap_or(0) as usize;
                let chunk = src["chunk_size"].as_u64().unwrap_or(0) as usize;
                let count = src["total_chunk_count"].as_u64().unwrap_or(0) as usize;
                // the media transfer guide's rules
                let fine = size > 0
                    && chunk > 0
                    && (1..=1000).contains(&count)
                    && count == size / chunk
                    && (chunk == size || (5 * MIB..=64 * MIB).contains(&chunk))
                    && size - (count - 1) * chunk <= 128 * MIB;
                if !fine {
                    return error(400, "invalid_param", &format!("chunks: size {size}, chunk {chunk}, count {count}"));
                }
                let pending = s.uploads.iter().filter(|u| !u.direct && u.status == "SEND_TO_USER_INBOX").count();
                if !direct && pending >= 5 {
                    return error(403, "spam_risk_too_many_pending_share", "Too many pending shares.");
                }
                let mut post_info = Value::Null;
                if direct {
                    let info = &body_json["post_info"];
                    let options: Vec<String> = s.creator["privacy_level_options"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| p.as_str().unwrap().to_string())
                        .collect();
                    let privacy = info["privacy_level"].as_str().unwrap_or_default();
                    if s.unaudited && options.iter().any(|o| o == "PUBLIC_TO_EVERYONE") {
                        return error(
                            403,
                            "unaudited_client_can_only_post_to_private_accounts",
                            "Please review our integration guidelines.",
                        );
                    }
                    if !options.iter().any(|o| o == privacy) {
                        return error(403, "privacy_level_option_mismatch", "privacy_level");
                    }
                    post_info = info.clone();
                }
                let n = s.uploads.len() + 1;
                let publish_id = format!("{}~v2.{n}", if direct { "p_pub_file" } else { "v_inbox_file" });
                s.uploads.push(Upload {
                    publish_id: publish_id.clone(),
                    direct,
                    size,
                    chunk_size: chunk,
                    count,
                    data: vec![],
                    post_info,
                    status: "PROCESSING_UPLOAD".into(),
                    fail_reason: String::new(),
                    post_ids: vec![],
                });
                let url = format!("{}/upload/?upload_id={n}&upload_token=fake-{n}", self.url);
                ok(json!({"publish_id": publish_id, "upload_url": url}))
            }
            "/v2/post/publish/status/fetch/" => {
                let id = body_json["publish_id"].as_str().unwrap_or_default();
                let Some(u) = s.uploads.iter().find(|u| u.publish_id == id) else {
                    return error(400, "invalid_publish_id", "publish_id does not exist");
                };
                let mut d = json!({"status": u.status, "uploaded_bytes": u.data.len()});
                if !u.fail_reason.is_empty() {
                    d["fail_reason"] = json!(u.fail_reason);
                }
                if !u.post_ids.is_empty() {
                    d["publicaly_available_post_id"] = json!(u.post_ids);
                }
                ok(d)
            }
            _ => error(404, "not_found", "no such endpoint in the fake"),
        }
    }

    fn chunk(
        &self,
        query: &HashMap<String, String>,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> (u16, Vec<(String, String)>, Value) {
        let mut s = self.state();
        let n: usize = query.get("upload_id").and_then(|n| n.parse().ok()).unwrap_or(0);
        let Some(u) = n.checked_sub(1).and_then(|i| s.uploads.get_mut(i)) else {
            return (404, vec![], Value::Null);
        };
        if query.get("upload_token") != Some(&format!("fake-{n}")) {
            return (403, vec![], Value::Null);
        }
        let kind = headers.get("content-type").map(String::as_str).unwrap_or_default();
        if !["video/mp4", "video/quicktime", "video/webm"].contains(&kind) {
            return (400, vec![], Value::Null);
        }
        let has = |u: &Upload| {
            if u.data.is_empty() {
                vec![]
            } else {
                vec![("Content-Range".to_string(), format!("bytes 0-{}/{}", u.data.len() - 1, u.size))]
            }
        };
        let Some((from, to, total)) = content_range(headers) else { return (400, vec![], Value::Null) };
        if total != u.size || from != u.data.len() || to + 1 != from + body.len() {
            return (416, has(u), Value::Null);
        }
        let i = from / u.chunk_size;
        let want = if i + 1 == u.count { u.size - from } else { u.chunk_size };
        if !from.is_multiple_of(u.chunk_size) || body.len() != want {
            return (400, vec![], Value::Null);
        }
        u.data.extend_from_slice(body);
        if u.data.len() < u.size {
            return (206, has(u), Value::Null);
        }
        u.status = if u.direct { "PUBLISH_COMPLETE".into() } else { "SEND_TO_USER_INBOX".into() };
        (201, has(u), Value::Null)
    }

    fn oauth(&self, f: &HashMap<String, String>) -> (u16, Vec<(String, String)>, Value) {
        let oops = |status: u16, error: &str| {
            (status, vec![], json!({"error": error, "error_description": error, "log_id": "fake-log"}))
        };
        if f.get("client_key").map(String::as_str) != Some(CLIENT_KEY)
            || f.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
        {
            return oops(401, "invalid_client");
        }
        let mut s = self.state();
        let refresh = match f.get("grant_type").map(String::as_str) {
            Some("authorization_code") => {
                if f.get("code").map(String::as_str) != Some(CODE)
                    || f.get("redirect_uri").map(String::as_str) != Some(REDIRECT)
                {
                    return oops(400, "invalid_grant");
                }
                let verifier = f.get("code_verifier").cloned().unwrap_or_default();
                if s.challenge.as_deref() != Some(sha256_hex(&verifier).as_str()) {
                    // TikTok's own answer to a verifier that doesn't match, with a 200
                    return oops(200, "invalid_grant");
                }
                REFRESH.to_string()
            }
            Some("refresh_token") => {
                let given = f.get("refresh_token").cloned().unwrap_or_default();
                if !s.refresh_tokens.contains(&given) {
                    return oops(400, "invalid_grant");
                }
                if s.rotate {
                    s.rotate = false;
                    s.refresh_tokens = vec![REFRESH_2.into()];
                    REFRESH_2.to_string()
                } else {
                    given
                }
            }
            _ => return oops(400, "unsupported_grant_type"),
        };
        let token = format!("act.fake-access-{}", s.tokens.len() + 1);
        s.tokens.push(token.clone());
        (
            200,
            vec![],
            json!({"access_token": token, "expires_in": s.expires_in, "open_id": "fake-open-id",
                   "refresh_expires_in": 31_536_000, "refresh_token": refresh, "scope": s.scopes,
                   "token_type": "Bearer"}),
        )
    }
}

/// `Content-Range: bytes <from>-<to>/<size>`.
pub fn content_range(headers: &HashMap<String, String>) -> Option<(usize, usize, usize)> {
    let r = headers.get("content-range")?.strip_prefix("bytes ")?;
    let (span, total) = r.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?, total.parse().ok()?))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Status",
    }
}

/// A query string or form body.
pub fn form(q: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(q.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// Bytes that look like nothing in particular: a video's stand-in.
pub fn bytes(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}
