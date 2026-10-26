//! The extension's process: the node starts it (`bin/tiktok`, from the extension's folder) and talks to it over stdin
//! and stdout. It makes videos ready for TikTok, sends them to a TikTok account as inbox drafts or direct posts, says
//! where they are, and signs accounts in from the apps. Nothing reaches TikTok without a sign-in in the vault, and no
//! video is sent without `confirm`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use pointman_extension::protocol::output_request::Role;
use pointman_extension::protocol::{CredentialsCodeRequest, CredentialsCodeResponse, CredentialsSignInRequest};
use pointman_extension::{Extension, Handle, Job};
use serde_json::{json, Value};
use tiktok::params::{check_caption, known};
use tiktok::signin::SignIns;
use tiktok::tiktok::{Api, Client, Endpoints};
use tiktok::upload::{self, Plan, Session, MIB};
use tiktok::v1::{
    AddParams, AddResult, ClientParams, ClientResult, CreatorParams, CreatorResult, Fit, Mode, PrepareParams,
    PrepareResult, Privacy, Settings, StatusParams, StatusResult, UploadParams, UploadResult,
};
use tiktok::{prepare, words, CLIENT_ITEM};

/// The longest video every creator may post, for `prepare` when the creator's own limit isn't to hand.
const ANYONE_MAY_POST: i32 = 180;

/// What a direct post's consent says, as TikTok's content sharing guidelines word it.
fn consent(branded: bool) -> &'static str {
    if branded {
        "By posting, you agree to TikTok's Branded Content Policy and Music Usage Confirmation."
    } else {
        "By posting, you agree to TikTok's Music Usage Confirmation."
    }
}

struct Cached {
    token: String,
    until: Instant,
}

struct Ext {
    h: Handle,
    agent: ureq::Agent,
    ends: Endpoints,
    cached: Mutex<Option<Cached>>,
    /// Refresh tokens TikTok gave in place of the vault's (it may rotate them, and an extension can't write the
    /// vault): by vault item, the vault's value they replace and the newer one. Memory only.
    rotated: Mutex<HashMap<String, (String, String)>>,
}

/// The creator's info, as tiktok.creator gives it.
struct Creator {
    username: String,
    nickname: String,
    privacy: Vec<Privacy>,
    max_seconds: i32,
    comments_off: bool,
    duets_off: bool,
    stitches_off: bool,
}

impl Ext {
    fn machine(&self) -> String {
        self.h.machine()
    }

    fn settings(&self) -> anyhow::Result<Settings> {
        self.h.typed_settings::<Settings>()
    }

    /// The vault item of the account's sign-in to use: the setting's, else the first by label.
    fn account(&self) -> anyhow::Result<String> {
        let accounts: Vec<String> = self.h.secrets("tiktok.")?.into_iter().filter(|n| n != CLIENT_ITEM).collect();
        let want = self.settings()?.account.trim().to_string();
        if !want.is_empty() {
            let name = format!("tiktok.{want}");
            if accounts.contains(&name) {
                return Ok(name);
            }
            let have = if accounts.is_empty() { "none".to_string() } else { accounts.join(", ") };
            bail!(
                "no TikTok sign-in {name} on {} (the account setting); it has {have}: sign it in with /tiktok add",
                self.machine()
            );
        }
        accounts.into_iter().min().ok_or_else(|| {
            anyhow!(
                "no TikTok account is signed in on {}: add the TikTok app (/tiktok client) if it isn't there, then \
                 type /tiktok add in the app and tap Sign in",
                self.machine()
            )
        })
    }

    /// The TikTok app's client key and secret, from the vault.
    fn client(&self) -> anyhow::Result<Client> {
        let Ok(value) = self.h.secret(CLIENT_ITEM) else {
            bail!(
                "no TikTok app on {}: type /tiktok client in the app and paste its client key and secret (TikTok for \
                 Developers › Manage apps)",
                self.machine()
            );
        };
        Client::parse(&value)
            .ok_or_else(|| anyhow!("{CLIENT_ITEM} isn't a client key and secret (the key, a space, then the secret)"))
    }

    /// An access token for the account, made again when it's within 5 minutes of running out, or `fresh`.
    fn token(&self, fresh: bool) -> anyhow::Result<String> {
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cached.as_ref().filter(|c| !fresh && c.until > Instant::now() + Duration::from_secs(300)) {
            return Ok(c.token.clone());
        }
        let name = self.account()?;
        let value = self.h.secret(&name)?.trim().to_string();
        if value.is_empty() || value.contains(char::is_whitespace) {
            bail!("{name} isn't a TikTok refresh token: sign the account in again with /tiktok add");
        }
        let client = self.client()?;
        let newer = self
            .rotated
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&name)
            .filter(|(was, _)| *was == value)
            .map(|(_, n)| n.clone());
        let (used, got) = match newer {
            // the newer one first; the vault's if TikTok no longer takes it
            Some(n) => match tiktok::tiktok::refresh(&self.agent, &self.ends, &client, &n) {
                Ok(t) => (n, t),
                Err(_) => (value.clone(), tiktok::tiktok::refresh(&self.agent, &self.ends, &client, &value)?),
            },
            None => (value.clone(), tiktok::tiktok::refresh(&self.agent, &self.ends, &client, &value)?),
        };
        if got.refresh != used {
            self.rotated.lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), (value, got.refresh.clone()));
            self.h.log(format!(
                "TikTok gave {name} a new refresh token: it's used from now on, in memory only (the vault keeps the one \
                 it was signed in with; sign in again with /tiktok add if TikTok stops taking that one)"
            ));
        }
        *cached = Some(Cached { token: got.access.clone(), until: Instant::now() + got.lasts });
        Ok(got.access)
    }

    fn api<T>(&self, f: impl FnOnce(&Api) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let token = |fresh: bool| self.token(fresh);
        f(&Api { agent: &self.agent, ends: &self.ends, token: &token })
    }

    /// The signed-in account, as TikTok lets it post now (creator_info/query).
    fn creator(&self) -> anyhow::Result<Creator> {
        let d = self.api(|api| api.post("post/publish/creator_info/query/", None))?;
        let text = |k: &str| d[k].as_str().unwrap_or_default().to_string();
        let privacy = d["privacy_level_options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().and_then(Privacy::from_tiktok))
            .collect();
        Ok(Creator {
            username: text("creator_username"),
            nickname: text("creator_nickname"),
            privacy,
            max_seconds: d["max_video_post_duration_sec"].as_i64().unwrap_or(0) as i32,
            comments_off: d["comment_disabled"].as_bool().unwrap_or(false),
            duets_off: d["duet_disabled"].as_bool().unwrap_or(false),
            stitches_off: d["stitch_disabled"].as_bool().unwrap_or(false),
        })
    }

    /// A file a command was given: a path in the folders the node lets the extension use (a board file the node
    /// fetched for `file` is in its state folder).
    fn file(&self, given: &str, what: &str) -> anyhow::Result<PathBuf> {
        let given = given.trim();
        if given.is_empty() {
            bail!("{what} is missing");
        }
        if given.contains("://") {
            bail!("{what}: {given} wasn't fetched by this node: give a board file (pointman://asset/<id>) or a path on it");
        }
        let path = self.h.allowed(given)?;
        if !path.is_file() {
            bail!("{what}: {} isn't a file", path.display());
        }
        Ok(path)
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn words_of(ps: &[Privacy]) -> String {
    let w: Vec<String> = ps.iter().map(|p| p.word()).collect();
    if w.is_empty() {
        "none".into()
    } else {
        w.join(", ")
    }
}

/// What the answers say until TikTok audits the app.
const UNAUDITED: &str = "Until TikTok audits the app, direct posts are self_only (seen only by the account's owner), \
                         and only while the account is set to private; drafts aren't held back.";

fn creator(x: &Ext, _: &Job, _: CreatorParams) -> anyhow::Result<CreatorResult> {
    let c = x.creator()?;
    let mut off: Vec<&str> = vec![];
    for (is_off, what) in [(c.comments_off, "comments"), (c.duets_off, "duets"), (c.stitches_off, "stitches")] {
        if is_off {
            off.push(what);
        }
    }
    let mut text = format!(
        "@{} ({}) can post videos up to {} s, for: {}.",
        c.username,
        c.nickname,
        c.max_seconds,
        words_of(&c.privacy)
    );
    if !off.is_empty() {
        text.push_str(&format!(" Its TikTok settings turn off {}.", words::and(&off)));
    }
    if !x.settings()?.audited {
        text.push(' ');
        text.push_str(UNAUDITED);
    }
    Ok(CreatorResult {
        text,
        privacy_options: c.privacy.iter().map(|p| (*p).into()).collect(),
        username: c.username,
        nickname: c.nickname,
        max_seconds: c.max_seconds,
        comments_off: c.comments_off,
        duets_off: c.duets_off,
        stitches_off: c.stitches_off,
        ..Default::default()
    })
}

fn prepare_video(x: &Ext, job: &Job, p: PrepareParams) -> anyhow::Result<PrepareResult> {
    let ffmpeg = prepare::tool("ffmpeg")?;
    let ffprobe = prepare::tool("ffprobe")?;
    let path = x.file(&p.file, "file")?;
    let fit = known(&p.fit).unwrap_or(Fit::FIT_PAD);
    let before = prepare::probe(&ffprobe, &path)?;
    let (max, whose) = if p.max_seconds > 0 {
        (p.max_seconds, "max_seconds".to_string())
    } else {
        match x.creator() {
            Ok(c) if c.max_seconds > 0 => (c.max_seconds, format!("@{}'s limit", c.username)),
            Ok(_) => (ANYONE_MAY_POST, "the limit every account has".to_string()),
            Err(e) => {
                job.handle().log(format!("prepare: no creator limit ({e:#}); using {ANYONE_MAY_POST} s"));
                (ANYONE_MAY_POST, "the limit every account has (the account's own wasn't to hand)".to_string())
            }
        }
    };
    let trimmed = before.seconds > max as f64 + 0.05;
    std::fs::create_dir_all(&job.work)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    let mut out = job.work.join(format!("{stem}-tiktok.mp4"));
    if out == path {
        out = job.work.join(format!("{stem}-tiktok-2.mp4"));
    }
    let args = prepare::args(&path, &out, &before, trimmed.then_some(max as f64), fit);
    let length = if trimmed { max as f64 } else { before.seconds };
    job.progress(0.0, format!("making {} ready for TikTok", name_of(&path)).as_str());
    prepare::run(job, &ffmpeg, &args, length)?;
    let after = prepare::probe(&ffprobe, &out)?;
    let bytes = std::fs::metadata(&out)?.len();
    job.output(
        &out,
        "video",
        Role::Main,
        json!({"width": after.width, "height": after.height, "seconds": after.seconds}),
    );

    let reshaped = if before.width * prepare::HEIGHT == before.height * prepare::WIDTH {
        String::new()
    } else if fit == Fit::FIT_CROP {
        ", cropped to 9:16".into()
    } else {
        ", padded to 9:16".into()
    };
    let cut = if trimmed { format!(", cut from {:.1} s to {max} s ({whose})", before.seconds) } else { String::new() };
    let text = format!(
        "Made {} ({}, {}x{}, {:.1} s, H.264/AAC{reshaped}{cut}) from {} ({}x{}). Send it with tiktok.upload file={}.",
        name_of(&out),
        words::size(bytes),
        after.width,
        after.height,
        after.seconds,
        name_of(&path),
        before.width,
        before.height,
        out.display()
    );
    Ok(PrepareResult {
        text,
        file: out.display().to_string(),
        width: after.width as i32,
        height: after.height as i32,
        seconds: after.seconds,
        trimmed,
        bytes: bytes as i64,
        ..Default::default()
    })
}

/// What a direct post sends as `post_info`, once its settings are allowed.
fn post_info(p: &UploadParams, privacy: Privacy, c: &Creator) -> anyhow::Result<Value> {
    if !c.privacy.contains(&privacy) {
        bail!(
            "privacy: {} isn't one @{} can post for now ({}): ask the owner for one of those",
            privacy.word(),
            c.username,
            words_of(&c.privacy)
        );
    }
    for (asked, off, what, field) in [
        (p.allow_comments, c.comments_off, "comments", "allow_comments"),
        (p.allow_duets, c.duets_off, "duets", "allow_duets"),
        (p.allow_stitches, c.stitches_off, "stitches", "allow_stitches"),
    ] {
        if asked && off {
            bail!("{field}: @{}'s TikTok settings turn {what} off, so it can't allow them: leave it out", c.username);
        }
    }
    let mut info = json!({
        "title": p.caption,
        "privacy_level": privacy.tiktok(),
        "disable_comment": !p.allow_comments,
        "disable_duet": !p.allow_duets,
        "disable_stitch": !p.allow_stitches,
        "brand_content_toggle": p.branded_content,
        "brand_organic_toggle": p.your_brand,
        "is_aigc": p.ai_generated,
    });
    if p.cover_ms > 0 {
        info["video_cover_timestamp_ms"] = json!(p.cover_ms);
    }
    Ok(info)
}

fn upload_video(x: &Ext, job: &Job, p: UploadParams) -> anyhow::Result<UploadResult> {
    let mode = known(&p.mode).unwrap_or(Mode::MODE_DRAFT);
    let direct = mode == Mode::MODE_DIRECT;
    let privacy = known(&p.privacy);
    let settings = x.settings()?;

    // what can be checked here first, so the owner is asked about what can go up
    check_caption(&p.caption)?;
    if direct {
        let Some(privacy) = privacy else {
            bail!(
                "privacy: a direct post goes up for the audience its owner picks, with none picked for them (TikTok's \
                 rule): ask them for one of tiktok.creator's privacy_options{}",
                if settings.audited { "" } else { " (until TikTok audits the app, only self_only)" }
            );
        };
        if !settings.audited && privacy != Privacy::PRIVACY_SELF_ONLY {
            bail!(
                "privacy: {} needs TikTok to have audited the app. {UNAUDITED} Send it as self_only or as a draft; once \
                 TikTok has audited the app, turn the audited setting on",
                privacy.word()
            );
        }
        if p.branded_content && privacy == Privacy::PRIVACY_SELF_ONLY {
            bail!("branded_content: TikTok doesn't let branded content be self_only");
        }
    }
    let path = x.file(&p.file, "file")?;
    upload::content_type(&path)?;
    let size = std::fs::metadata(&path)?.len();
    let plan = Plan::new(size, settings.chunk_mib.clamp(5, 64) as u64 * MIB)?;

    let what = match (direct, privacy) {
        (true, Some(pr)) => format!("a direct post for {}", pr.word()),
        _ => "a draft in the account's TikTok inbox".to_string(),
    };
    if !p.confirm {
        let consent = if direct { format!(" {}", consent(p.branded_content)) } else { String::new() };
        bail!(
            "not sent: tiktok.upload needs confirm=true, given only once the account's owner has said yes to sending \
             {} ({}) as {what}, captioned \"{}\".{consent}",
            name_of(&path),
            words::size(size),
            words::start(&p.caption, 80)
        );
    }

    let mut text = String::new();
    let (info, who) = if direct {
        let c = x.creator()?;
        let info = post_info(&p, privacy.expect("checked"), &c)?;
        match prepare::tool("ffprobe").and_then(|f| prepare::probe(&f, &path)) {
            Ok(probe) if c.max_seconds > 0 && probe.seconds > c.max_seconds as f64 + 0.05 => bail!(
                "it's {:.1} s, and @{} may post at most {} s: tiktok.prepare cuts it to that",
                probe.seconds,
                c.username,
                c.max_seconds
            ),
            Ok(_) => {}
            Err(e) => job.handle().log(format!("couldn't check its length before posting ({e:#}); TikTok will")),
        }
        (Some(info), format!(" to @{}", c.username))
    } else {
        (None, String::new())
    };

    let mode_word = mode.word();
    let resume = job.checkpoint.clone().and_then(|c| serde_json::from_value::<Session>(c).ok());
    let session = match resume.filter(|s| upload::still(s, &path, &plan, mode_word)) {
        Some(s) => {
            job.handle().log(format!("{}: carrying on with TikTok's upload {}", name_of(&path), s.publish_id));
            s
        }
        None => {
            job.progress(0.0, format!("starting the upload of {} ({})", name_of(&path), words::size(size)).as_str());
            let (publish_id, upload_url) = x.api(|api| upload::init(api, info.as_ref(), &plan))?;
            let s = Session {
                publish_id,
                upload_url,
                mode: mode_word.to_string(),
                file: path.display().to_string(),
                modified: upload::modified(&path),
                plan,
                sent: 0,
                at: upload::now(),
            };
            job.save(&s);
            s
        }
    };
    let started = Instant::now();
    let s = upload::chunks(job, &x.agent, &path, session)?;
    let st = x.api(|api| upload::settle(job, api, &s.publish_id, Duration::from_secs(60), Duration::from_secs(5)))?;
    let status = st["status"].as_str().unwrap_or_default().to_string();
    if status == "FAILED" {
        let why = st["fail_reason"].as_str().unwrap_or("no reason given");
        let fix = if why.ends_with("_check_failed") { ": tiktok.prepare makes a file TikTok takes" } else { "" };
        bail!("TikTok took {} but failed it ({}): {why}{fix}", name_of(&path), s.publish_id);
    }
    text.push_str(&format!(
        "Sent {} ({}, in {} s){who} as {what}: {}.",
        name_of(&path),
        words::size(size),
        started.elapsed().as_secs(),
        if status.is_empty() { "status unknown" } else { status.as_str() }
    ));
    if direct {
        if !settings.audited {
            text.push_str(" Until TikTok audits the app, it's seen only by the account's owner.");
        }
        text.push_str(&format!(
            " TikTok takes a few minutes before it's on the profile: tiktok.status publish_id={}.",
            s.publish_id
        ));
    } else {
        text.push_str(" Its owner finishes and posts it from the notification in TikTok's inbox, in the app");
        if p.caption.trim().is_empty() {
            text.push('.');
        } else {
            text.push_str(&format!(". TikTok's inbox takes no caption, so paste this one there: {}", p.caption.trim()));
        }
        let unused: Vec<&str> = [
            (privacy.is_some(), "privacy"),
            (p.allow_comments || p.allow_duets || p.allow_stitches, "who may comment, duet or stitch"),
            (p.cover_ms > 0, "the cover"),
            (p.your_brand || p.branded_content, "the brand disclosure"),
            (p.ai_generated, "the AI-generated label"),
        ]
        .into_iter()
        .filter_map(|(given, what)| given.then_some(what))
        .collect();
        if !unused.is_empty() {
            text.push_str(&format!(" (A draft's {} are chosen in the app too.)", words::and(&unused)));
        }
    }
    Ok(UploadResult {
        text,
        publish_id: s.publish_id,
        mode: mode.into(),
        status,
        privacy: privacy.filter(|_| direct).unwrap_or_default().into(),
        bytes: size as i64,
        ..Default::default()
    })
}

fn status(x: &Ext, _: &Job, p: StatusParams) -> anyhow::Result<StatusResult> {
    let id = p.publish_id.trim().to_string();
    if id.is_empty() || id.len() > 64 {
        bail!("publish_id: tiktok.upload's publish_id, at most 64 characters");
    }
    let d = x.api(|api| upload::status(api, &id))?;
    let status = d["status"].as_str().unwrap_or_default().to_string();
    let fail_reason = d["fail_reason"].as_str().unwrap_or_default().to_string();
    // TikTok's own spelling
    let post_ids: Vec<String> = d["publicaly_available_post_id"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_u64().map(|n| n.to_string()).or_else(|| v.as_str().map(String::from)))
        .collect();
    let uploaded = d["uploaded_bytes"].as_i64().unwrap_or(0);
    let text = match status.as_str() {
        "PROCESSING_UPLOAD" => format!("TikTok is still taking the upload ({} so far).", words::size(uploaded as u64)),
        "PROCESSING_DOWNLOAD" => "TikTok is still fetching it.".to_string(),
        "SEND_TO_USER_INBOX" => {
            "It's in the account's TikTok inbox, for its owner to finish and post from the notification in the app."
                .to_string()
        }
        "PUBLISH_COMPLETE" if post_ids.is_empty() => {
            "Posted. It isn't public (yet): TikTok lists a post's id once it's public and has passed moderation."
                .to_string()
        }
        "PUBLISH_COMPLETE" => format!("Posted, and public: post {}.", post_ids.join(", ")),
        "FAILED" => {
            let fix =
                if fail_reason.ends_with("_check_failed") { " (tiktok.prepare makes a file TikTok takes)" } else { "" };
            format!("It failed: {}{fix}.", if fail_reason.is_empty() { "no reason given" } else { &fail_reason })
        }
        other => format!("TikTok says {other}."),
    };
    Ok(StatusResult {
        text,
        publish_id: id,
        status,
        fail_reason,
        post_ids,
        uploaded_bytes: uploaded,
        ..Default::default()
    })
}

fn main() -> anyhow::Result<()> {
    let mut ext = Extension::new()?;
    let x = Arc::new(Ext {
        h: ext.handle(),
        agent: tiktok::tiktok::agent(),
        ends: Endpoints::from_env(),
        cached: Mutex::new(None),
        rotated: Mutex::new(HashMap::new()),
    });
    let signins = Arc::new(SignIns::default());

    let xx = x.clone();
    ext.typed_command(move |job: &Job, p: CreatorParams| creator(&xx, job, p));
    let xx = x.clone();
    ext.typed_command(move |job: &Job, p: PrepareParams| prepare_video(&xx, job, p));
    let xx = x.clone();
    ext.typed_command(move |job: &Job, p: UploadParams| upload_video(&xx, job, p));
    let xx = x.clone();
    ext.typed_command(move |job: &Job, p: StatusParams| status(&xx, job, p));

    // `/tiktok add` and `/tiktok client` posted (an agent, an older app): the apps open the forms for them
    ext.typed_command(|_: &Job, _: AddParams| {
        Ok(AddResult {
            text: "A TikTok account is signed in from the Pointman app, so its token never goes in a post: type \
                   `/tiktok add` there and tap Sign in."
                .into(),
            ..Default::default()
        })
    });
    ext.typed_command(|_: &Job, _: ClientParams| {
        Ok(ClientResult {
            text:
                "The TikTok app's client key and secret are added in the Pointman app, so the secret never goes in a \
                   post: type `/tiktok client` there and paste them."
                    .into(),
            ..Default::default()
        })
    });

    // Sign in from the apps: Login Kit's authorization code with PKCE, its redirect address pasted back
    let (xx, s) = (x.clone(), signins.clone());
    ext.on(move |p: CredentialsSignInRequest| {
        if p.form != "account" {
            bail!("the TikTok extension signs in accounts only (form: account)");
        }
        if p.label.trim() == CLIENT_ITEM.trim_start_matches("tiktok.") {
            bail!("{CLIENT_ITEM} is the TikTok app's place in the vault: call the account something else");
        }
        let session = p.session.as_option().map(|s| s.value.as_str()).unwrap_or_default();
        let settings = xx.settings()?;
        s.start(xx.client()?, &settings.redirect, settings.direct_post, session)
    });
    let (xx, s) = (x.clone(), signins);
    ext.on(move |p: CredentialsCodeRequest| {
        let session = p.session.as_option().map(|s| s.value.as_str()).unwrap_or_default();
        let value = s.code(&xx.agent, &xx.ends, session, &p.code)?;
        Ok(CredentialsCodeResponse { value, ..Default::default() })
    });

    ext.run()
}
