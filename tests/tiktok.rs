//! The extension's commands under a stand-in node, against a fake TikTok (tests/common): the paste-back sign-in,
//! tokens made again before they run out, drafts and direct posts sent in TikTok's chunks, nothing reaching TikTok
//! without `confirm`, and videos made ready with ffmpeg. No real account is called: the extension is pointed at the
//! fake through TIKTOK_API_BASE.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Fake, CLIENT_KEY, CLIENT_SECRET, CODE, MIB, REDIRECT, REFRESH, REFRESH_2};
use pointman_extension::testing::{RunOptions, StandIn};
use serde_json::{json, Value};

fn node_with(fake: &Fake, dir: &Path, settings: Value, secrets: Vec<(&str, String)>) -> StandIn {
    StandIn::new(env!("CARGO_MANIFEST_DIR"))
        .binary(env!("CARGO_BIN_EXE_tiktok"))
        .secrets(secrets)
        .settings(settings)
        .env("TIKTOK_API_BASE", &fake.url)
        .roots([dir.to_path_buf()])
        .start()
        .unwrap()
}

fn client() -> (&'static str, String) {
    ("tiktok.client", format!("{CLIENT_KEY} {CLIENT_SECRET}"))
}

fn signed_in() -> Vec<(&'static str, String)> {
    vec![client(), ("tiktok.main", REFRESH.to_string())]
}

fn node(fake: &Fake, dir: &Path) -> StandIn {
    node_with(fake, dir, json!({"chunk_mib": 5}), signed_in())
}

fn video_file(dir: &Path, name: &str, size: usize) -> (PathBuf, Vec<u8>) {
    let data = common::bytes(size);
    let path = dir.join(name);
    std::fs::write(&path, &data).unwrap();
    (path, data)
}

/// The query of a sign-in's authorize link.
fn query(url: &str) -> std::collections::HashMap<String, String> {
    common::form(url.split_once('?').unwrap().1)
}

#[test]
fn a_sign_in_takes_the_pasted_address_and_refuses_another_sign_ins() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(&fake, dir.path(), json!({}), vec![client()]);
    let start = |session: &str| {
        node.call(
            "credentials.sign_in",
            json!({"form": "account", "label": "main", "person": {"name": "james"}, "session": {"id": session}}),
        )
        .unwrap()
    };
    let first = start("sgn_1");
    let url = first["url"].as_str().unwrap();
    assert!(url.starts_with("https://www.tiktok.com/v2/auth/authorize/?"), "{url}");
    assert!(first.get("waits").is_none() || first["waits"] == false, "nothing to wait for: the address is pasted");
    assert!(first["prompt"].as_str().unwrap().contains("whole address"), "{first}");
    let q = query(url);
    assert_eq!(q["client_key"], CLIENT_KEY);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["scope"], "user.info.basic,video.upload,video.publish");
    assert_eq!(q["redirect_uri"], REDIRECT, "the default redirect, as registered in the TikTok app");
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(
        q["code_challenge"].len() == 64 && q["code_challenge"].chars().all(|c| c.is_ascii_hexdigit()),
        "hex SHA-256"
    );
    assert!(!url.contains(CLIENT_SECRET), "the secret never goes in the link");
    let second = start("sgn_2");
    let q2 = query(second["url"].as_str().unwrap());
    assert_ne!(q["state"], q2["state"]);
    assert_ne!(q["code_challenge"], q2["code_challenge"], "a new verifier each time");

    // the address another sign-in ended on is refused before TikTok is asked, and this sign-in ends
    let theirs = format!("{REDIRECT}?code={}&scopes=video.upload&state={}", "fake-code%2A123%21", q2["state"]);
    let e = node
        .call("credentials.code", json!({"form": "account", "session": {"id": "sgn_1"}, "code": theirs}))
        .unwrap_err();
    assert!(e.message.contains("another sign-in") && e.message.contains("refused"), "{e}");
    assert!(fake.requests().is_empty(), "no code was traded: {:?}", fake.paths());
    let e = node
        .call("credentials.code", json!({"form": "account", "session": {"id": "sgn_1"}, "code": theirs}))
        .unwrap_err();
    assert!(e.message.contains("ended"), "{e}");

    // something that isn't the address: the sign-in carries on for the right paste
    let e = node
        .call("credentials.code", json!({"form": "account", "session": {"id": "sgn_2"}, "code": "WDJB-MJHT"}))
        .unwrap_err();
    assert!(e.message.contains("paste all of it"), "{e}");
    fake.state().challenge = Some(q2["code_challenge"].clone());
    let ours =
        format!("  {REDIRECT}?code=fake-code%2A123%21&scopes=user.info.basic%2Cvideo.upload&state={}  ", q2["state"]);
    let got =
        node.call("credentials.code", json!({"form": "account", "session": {"id": "sgn_2"}, "code": ours})).unwrap();
    assert_eq!(got["value"], REFRESH, "the vault's value is the refresh token alone");
    let traded = fake.requests().into_iter().find(|r| r.path == "/v2/oauth/token/").unwrap();
    assert_eq!(traded.form["grant_type"], "authorization_code");
    assert_eq!(traded.form["code"], CODE, "the code, URL-decoded");
    assert_eq!(traded.form["redirect_uri"], REDIRECT);
    assert_eq!(traded.form["code_verifier"].len(), 64, "the fake checked its SHA-256 was the challenge");

    // TikTok turned it down, or the TikTok app isn't added yet
    start("sgn_3");
    let e = node
        .call(
            "credentials.code",
            json!({"form": "account", "session": {"id": "sgn_3"},
                   "code": format!("{REDIRECT}?error=access_denied&error_description=User+cancelled+the+authorization")}),
        )
        .unwrap_err();
    assert!(e.message.contains("access_denied") && e.message.contains("User cancelled"), "{e}");
    let e = node
        .call("credentials.sign_in", json!({"form": "account", "label": "client", "session": {"id": "sgn_4"}}))
        .unwrap_err();
    assert!(e.message.contains("tiktok.client"), "{e}");
    let bare = node_with(&fake, dir.path(), json!({}), vec![]);
    let e = bare
        .call("credentials.sign_in", json!({"form": "account", "label": "main", "session": {"id": "sgn_5"}}))
        .unwrap_err();
    assert!(e.message.contains("/tiktok client"), "{e}");
}

#[test]
fn a_sign_in_without_the_scopes_it_asked_for_is_refused() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let node = node_with(&fake, dir.path(), json!({}), vec![client()]);
    let sign_in = |node: &StandIn, session: &str| {
        let started = node
            .call("credentials.sign_in", json!({"form": "account", "label": "main", "session": {"id": session}}))
            .unwrap();
        let q = query(started["url"].as_str().unwrap());
        fake.state().challenge = Some(q["code_challenge"].clone());
        let pasted = format!("{REDIRECT}?code=fake-code%2A123%21&state={}", q["state"]);
        node.call("credentials.code", json!({"form": "account", "session": {"id": session}, "code": pasted}))
    };
    fake.state().scopes = "user.info.basic,video.upload".into();
    let e = sign_in(&node, "sgn_1").unwrap_err();
    assert!(e.message.contains("video.publish") && e.message.contains("direct_post"), "{e}");
    // drafts only: it asks for no more than it needs, and takes that
    let drafts = node_with(&fake, dir.path(), json!({"direct_post": false}), vec![client()]);
    assert_eq!(sign_in(&drafts, "sgn_2").unwrap()["value"], REFRESH);
    fake.state().scopes = "user.info.basic".into();
    let e = sign_in(&drafts, "sgn_3").unwrap_err();
    assert!(e.message.contains("video.upload"), "{e}");
}

#[test]
fn a_token_is_made_again_before_it_runs_out_when_tiktok_refuses_it_and_when_tiktok_rotates_it() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let node = node(&fake, dir.path());
    let refreshes = |f: &Fake| f.requests().iter().filter(|r| r.path == "/v2/oauth/token/").count();

    // TikTok's tokens last a day: one refresh, then the same token
    node.run("tiktok.creator", json!({})).unwrap();
    node.run("tiktok.creator", json!({})).unwrap();
    assert_eq!(refreshes(&fake), 1);
    let refresh = fake.requests().into_iter().find(|r| r.path == "/v2/oauth/token/").unwrap();
    assert_eq!(refresh.form["grant_type"], "refresh_token");
    assert_eq!(refresh.form["refresh_token"], REFRESH);

    // TikTok says the token it gave is invalid: a new one, and the call again
    let first = fake.state().tokens[0].clone();
    fake.state().expire.push(first.clone());
    node.run("tiktok.creator", json!({})).unwrap();
    assert_eq!(refreshes(&fake), 2);

    // one within 5 minutes of running out is made again before it's used
    let short = node_with(&fake, dir.path(), json!({}), signed_in());
    fake.state().expires_in = 240;
    short.run("tiktok.creator", json!({})).unwrap();
    short.run("tiktok.creator", json!({})).unwrap();
    assert_eq!(refreshes(&fake), 4);

    // TikTok rotates the refresh token, and stops taking the old one: the new one is used from then on
    fake.state().rotate = true;
    short.run("tiktok.creator", json!({})).unwrap();
    short.run("tiktok.creator", json!({})).unwrap();
    let sent: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.path == "/v2/oauth/token/")
        .map(|r| r.form["refresh_token"].clone())
        .collect();
    assert_eq!(sent[4..], [REFRESH.to_string(), REFRESH_2.to_string()]);
    assert!(short.log().contains("new refresh token"), "{}", short.log());
    // a node started after that has only the vault's, which TikTok no longer takes: it says to sign in again
    let e = node_with(&fake, dir.path(), json!({}), signed_in()).run("tiktok.creator", json!({})).unwrap_err();
    assert!(e.message.contains("invalid_grant") && e.message.contains("/tiktok add"), "{e}");

    let said = format!("{}\n{}", node.log(), short.log());
    for secret in [REFRESH, REFRESH_2, CLIENT_SECRET, first.as_str()] {
        assert!(!said.contains(secret), "{secret} is in a log");
    }
}

#[test]
fn creator_says_who_the_account_can_post_for() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let node = node(&fake, dir.path());
    let got = node.run("tiktok.creator", json!({})).unwrap().result;
    assert_eq!(got["username"], "jamescalam");
    assert_eq!(got["privacy_options"], json!(["public_to_everyone", "mutual_follow_friends", "self_only"]));
    assert_eq!(got["max_seconds"], 600);
    assert_eq!(got["stitches_off"], true);
    let text = got["text"].as_str().unwrap();
    assert!(text.contains("@jamescalam") && text.contains("turn off stitches"), "{text}");
    assert!(text.contains("Until TikTok audits the app"), "{text}");
    let audited = node_with(&fake, dir.path(), json!({"audited": true}), signed_in());
    let text = audited.run("tiktok.creator", json!({})).unwrap().result["text"].as_str().unwrap().to_string();
    assert!(!text.contains("audits"), "{text}");

    // nothing signed in: it says how
    let none = node_with(&fake, dir.path(), json!({}), vec![client()]);
    let e = none.run("tiktok.creator", json!({})).unwrap_err();
    assert!(e.message.contains("/tiktok add"), "{e}");
    let e = node_with(&fake, dir.path(), json!({"account": "work"}), signed_in())
        .run("tiktok.creator", json!({}))
        .unwrap_err();
    assert!(e.message.contains("tiktok.work") && e.message.contains("tiktok.main"), "{e}");
}

#[test]
fn nothing_reaches_tiktok_without_confirm_or_with_what_tiktok_would_refuse() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = video_file(dir.path(), "short.mp4", 300_000);
    std::fs::write(dir.path().join("cut.mkv"), b"x").unwrap();
    let node = node(&fake, dir.path());
    let caption = "Claude Code in 60 seconds #ai #coding";
    for confirm in [json!(null), json!(false)] {
        let mut draft = json!({"file": path, "caption": caption});
        let mut direct = json!({"file": path, "caption": caption, "mode": "direct", "privacy": "self_only"});
        if !confirm.is_null() {
            draft["confirm"] = confirm.clone();
            direct["confirm"] = confirm;
        }
        let e = node.run("tiktok.upload", draft).unwrap_err();
        assert!(e.message.contains("confirm=true") && e.message.contains("draft in the account's TikTok inbox"), "{e}");
        assert!(e.message.contains("short.mp4") && e.message.contains("Claude Code in 60 seconds"), "{e}");
        let e = node.run("tiktok.upload", direct).unwrap_err();
        assert!(e.message.contains("confirm=true") && e.message.contains("direct post for self_only"), "{e}");
        assert!(e.message.contains("Music Usage Confirmation"), "the consent TikTok asks for: {e}");
    }
    let refused = [
        (json!({"file": path, "mode": "direct", "confirm": true}), "none picked for them"),
        (json!({"file": path, "mode": "direct", "privacy": "public_to_everyone", "confirm": true}), "audited"),
        (json!({"file": path, "caption": "Full video: https://youtu.be/abcdefghijk", "confirm": true}), "strips links"),
        (json!({"file": path, "caption": "x".repeat(2201), "confirm": true}), "2,200"),
        (json!({"file": path, "mode": "public", "confirm": true}), "isn't one of draft, direct"),
        (json!({"file": path, "titel": "typo", "confirm": true}), "titel isn't a field"),
        (json!({"file": dir.path().join("cut.mkv"), "confirm": true}), "MP4, MOV or WebM"),
    ];
    for (params, why) in refused {
        let e = node.run("tiktok.upload", params.clone()).unwrap_err();
        assert!(e.message.contains(why), "{params}: {e}");
    }
    let audited = node_with(&fake, dir.path(), json!({"audited": true}), signed_in());
    let e = audited
        .run(
            "tiktok.upload",
            json!({"file": path, "mode": "direct", "privacy": "self_only", "branded_content": true, "confirm": true}),
        )
        .unwrap_err();
    assert!(e.message.contains("branded content"), "{e}");
    assert!(fake.requests().is_empty(), "no token asked for, nothing sent: {:?}", fake.paths());
}

#[test]
fn a_draft_goes_up_in_tiktoks_chunks_to_the_inbox_after_a_dropped_connection() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let size = 11 * MIB + 123;
    let (path, data) = video_file(dir.path(), "short.mp4", size);
    // the connection drops 1 MiB into the second chunk: TikTok keeps none of it, and it's sent again
    fake.state().drop_at = Some(6 * MIB);
    let node = node(&fake, dir.path());
    let caption = "Claude Code in 60 seconds #ai #coding";
    let done = node
        .run_timeout(
            "tiktok.upload",
            json!({"file": path, "caption": caption, "confirm": true}),
            Duration::from_secs(60),
        )
        .unwrap();
    let r = &done.result;
    assert_eq!(r["mode"], "draft", "a draft unless said");
    assert_eq!(r["status"], "SEND_TO_USER_INBOX");
    assert_eq!(r["bytes"], size as u64);
    assert!(r["publish_id"].as_str().unwrap().starts_with("v_inbox_file~"));
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("TikTok's inbox") && text.contains(caption), "the caption to paste in the app: {text}");

    let s = fake.state();
    assert_eq!(s.uploads.len(), 1);
    let up = &s.uploads[0];
    assert!(up.data == data, "TikTok has the file byte for byte");
    assert_eq!((up.chunk_size, up.count), (5 * MIB, 2), "5 MiB, and the rest (6 MiB and 123 bytes) in the last");
    drop(s);
    let init = fake.requests().into_iter().find(|r| r.path == "/v2/post/publish/inbox/video/init/").unwrap();
    assert_eq!(
        init.json,
        json!({"source_info": {"source": "FILE_UPLOAD", "video_size": size, "chunk_size": 5 * MIB, "total_chunk_count": 2}}),
        "no post_info: the inbox takes none"
    );
    let puts: Vec<_> = fake.requests().into_iter().filter(|r| r.method == "PUT").collect();
    let ranges: Vec<String> = puts.iter().map(|r| r.headers["content-range"].clone()).collect();
    assert_eq!(
        ranges,
        [
            format!("bytes 0-{}/{size}", 5 * MIB - 1),
            format!("bytes {}-{}/{size}", 5 * MIB, size - 1),
            format!("bytes {}-{}/{size}", 5 * MIB, size - 1),
        ]
    );
    assert_eq!(puts[1].read, MIB, "the second chunk's connection dropped partway");
    assert!(puts.iter().all(|p| p.headers["content-type"] == "video/mp4" && !p.headers.contains_key("authorization")));
    assert!(node.log().contains("sending chunk 2 again"), "{}", node.log());
    let stages: Vec<String> = done.progress.iter().filter_map(|p| p.stage.clone()).collect();
    assert!(stages.iter().any(|s| s.contains("uploading short.mp4")), "{stages:?}");
    assert!(!done.checkpoints.is_empty(), "the upload is its checkpoint");

    // TikTok's own refusal says what to do
    for _ in 0..4 {
        let (p, _) = video_file(dir.path(), "small.mp4", 1000);
        node.run("tiktok.upload", json!({"file": p, "confirm": true})).unwrap();
    }
    let e = node.run("tiktok.upload", json!({"file": path, "confirm": true})).unwrap_err();
    assert!(e.message.contains("spam_risk_too_many_pending_share") && e.message.contains("5 drafts"), "{e}");
}

#[test]
fn an_upload_carries_on_from_its_checkpoint_after_a_restart() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let size = 10 * MIB + 5;
    let (path, data) = video_file(dir.path(), "short.mp4", size);
    let node = node(&fake, dir.path());
    // the first run's upload, with its first chunk at TikTok, and the checkpoint it saved
    let done =
        node.run_timeout("tiktok.upload", json!({"file": path, "confirm": true}), Duration::from_secs(60)).unwrap();
    let first = done.checkpoints[0].clone();
    assert_eq!(first["sent"], 0);
    {
        let mut s = fake.state();
        s.uploads.push(common::Upload {
            publish_id: "v_inbox_file~v2.2".into(),
            direct: false,
            size,
            chunk_size: 5 * MIB,
            count: 2,
            data: data[..5 * MIB].to_vec(),
            post_info: Value::Null,
            status: "PROCESSING_UPLOAD".into(),
            fail_reason: String::new(),
            post_ids: vec![],
        });
    }
    let mut checkpoint = first.clone();
    checkpoint["publish_id"] = json!("v_inbox_file~v2.2");
    checkpoint["upload_url"] = json!(format!("{}/upload/?upload_id=2&upload_token=fake-2", fake.url));
    checkpoint["sent"] = json!(1);
    let before = fake.requests().len();
    let done = node
        .run_with(
            "tiktok.upload",
            json!({"file": path, "confirm": true}),
            RunOptions { checkpoint: Some(checkpoint), timeout: Duration::from_secs(60), ..Default::default() },
        )
        .unwrap();
    assert_eq!(done.result["publish_id"], "v_inbox_file~v2.2");
    assert!(fake.state().uploads[1].data == data);
    let after: Vec<_> = fake.requests()[before..].to_vec();
    assert!(!after.iter().any(|r| r.path.ends_with("/init/")), "no new upload");
    let ranges: Vec<&str> =
        after.iter().filter(|r| r.method == "PUT").map(|r| r.headers["content-range"].as_str()).collect();
    assert_eq!(ranges, [format!("bytes {}-{}/{size}", 5 * MIB, size - 1)]);
}

#[test]
fn a_direct_post_asks_the_creator_first_and_keeps_to_what_it_allows() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let (path, data) = video_file(dir.path(), "short.mp4", 400_000);
    let node = node(&fake, dir.path());
    let done = node
        .run(
            "tiktok.upload",
            json!({"file": path, "caption": "Claude Code #ai", "mode": "direct", "privacy": "self_only",
                   "allow_comments": true, "ai_generated": true, "cover_ms": 1500, "confirm": true}),
        )
        .unwrap();
    let r = &done.result;
    assert_eq!(
        (r["mode"].as_str(), r["status"].as_str(), r["privacy"].as_str()),
        (Some("direct"), Some("PUBLISH_COMPLETE"), Some("self_only"))
    );
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("@jamescalam") && text.contains("seen only by the account's owner"), "{text}");
    let paths = fake.paths();
    let creator = paths.iter().position(|p| p == "/v2/post/publish/creator_info/query/").unwrap();
    let init = paths.iter().position(|p| p == "/v2/post/publish/video/init/").unwrap();
    assert!(creator < init, "the creator's info first, as TikTok asks: {paths:?}");
    let s = fake.state();
    assert!(s.uploads[0].data == data);
    assert_eq!(
        s.uploads[0].post_info,
        json!({"title": "Claude Code #ai", "privacy_level": "SELF_ONLY", "disable_comment": false,
               "disable_duet": true, "disable_stitch": true, "brand_content_toggle": false,
               "brand_organic_toggle": false, "is_aigc": true, "video_cover_timestamp_ms": 1500}),
        "interactions off unless the owner said"
    );
    drop(s);

    // what the creator's settings don't allow is refused before an upload starts
    let audited = node_with(&fake, dir.path(), json!({"audited": true}), signed_in());
    let before = fake.state().uploads.len();
    let e = audited
        .run(
            "tiktok.upload",
            json!({"file": path, "mode": "direct", "privacy": "self_only", "allow_stitches": true, "confirm": true}),
        )
        .unwrap_err();
    assert!(e.message.contains("stitches off"), "{e}");
    fake.state().creator["privacy_level_options"] =
        json!(["FOLLOWER_OF_CREATOR", "MUTUAL_FOLLOW_FRIENDS", "SELF_ONLY"]);
    let e = audited
        .run("tiktok.upload", json!({"file": path, "mode": "direct", "privacy": "public_to_everyone", "confirm": true}))
        .unwrap_err();
    assert!(e.message.contains("follower_of_creator, mutual_follow_friends, self_only"), "{e}");
    assert_eq!(fake.state().uploads.len(), before);

    // TikTok's refusal of an unaudited app posting to a public account says what to do
    fake.state().creator["privacy_level_options"] = json!(["PUBLIC_TO_EVERYONE", "MUTUAL_FOLLOW_FRIENDS", "SELF_ONLY"]);
    fake.state().unaudited = true;
    let e = node
        .run("tiktok.upload", json!({"file": path, "mode": "direct", "privacy": "self_only", "confirm": true}))
        .unwrap_err();
    assert!(
        e.message.contains("unaudited_client_can_only_post_to_private_accounts")
            && e.message.contains("set to private"),
        "{e}"
    );
}

#[test]
fn status_says_where_a_sent_video_is() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = video_file(dir.path(), "short.mp4", 1000);
    let node = node(&fake, dir.path());
    let id = node.run("tiktok.upload", json!({"file": path, "confirm": true})).unwrap().result["publish_id"]
        .as_str()
        .unwrap()
        .to_string();
    let got = node.run("tiktok.status", json!({"publish_id": id})).unwrap().result;
    assert_eq!(got["status"], "SEND_TO_USER_INBOX");
    assert!(got["text"].as_str().unwrap().contains("inbox"), "{got}");

    fake.state().uploads[0].status = "PUBLISH_COMPLETE".into();
    fake.state().uploads[0].post_ids = vec![7_431_234_567_890_123_456];
    let got = node.run("tiktok.status", json!({"publish_id": id})).unwrap().result;
    assert_eq!(got["post_ids"], json!(["7431234567890123456"]), "64-bit ids, as text");
    assert!(got["text"].as_str().unwrap().contains("public"), "{got}");

    fake.state().uploads[0].status = "FAILED".into();
    fake.state().uploads[0].fail_reason = "duration_check_failed".into();
    let got = node.run("tiktok.status", json!({"publish_id": id})).unwrap().result;
    assert_eq!(got["fail_reason"], "duration_check_failed");
    assert!(got["text"].as_str().unwrap().contains("tiktok.prepare"), "{got}");

    let e = node.run("tiktok.status", json!({"publish_id": "v_inbox_file~nope"})).unwrap_err();
    assert!(e.message.contains("invalid_publish_id"), "{e}");
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg is on the PATH");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn probe(path: &Path) -> Value {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A landscape test card with a tone: what a camera or screen recording gives.
fn test_video(dir: &Path, name: &str, seconds: u32, audio: bool) -> PathBuf {
    let path = dir.join(name);
    let length = seconds.to_string();
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30"];
    if audio {
        args.extend(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100"]);
    }
    args.extend(["-t", &length, "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"]);
    if audio {
        args.extend(["-c:a", "aac"]);
    }
    let out = path.display().to_string();
    args.push(&out);
    ffmpeg(&args);
    path
}

#[test]
fn prepare_makes_a_9_16_h264_mp4_no_longer_than_the_account_may_post() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let source = test_video(dir.path(), "landscape.mov", 5, true);
    fake.state().creator["max_video_post_duration_sec"] = json!(3);
    let node = node(&fake, dir.path());
    let done = node.run_timeout("tiktok.prepare", json!({"file": source}), Duration::from_secs(120)).unwrap();
    let r = &done.result;
    assert_eq!((r["width"].as_i64(), r["height"].as_i64()), (Some(1080), Some(1920)));
    assert_eq!(r["trimmed"], true);
    assert!((r["seconds"].as_f64().unwrap() - 3.0).abs() < 0.2, "{r}");
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("padded to 9:16") && text.contains("@jamescalam's limit"), "{text}");
    let out = PathBuf::from(r["file"].as_str().unwrap());
    assert!(out.ends_with("landscape-tiktok.mp4"), "{}", out.display());
    let p = probe(&out);
    let streams = p["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!((video["codec_name"].as_str(), video["pix_fmt"].as_str()), (Some("h264"), Some("yuv420p")));
    assert_eq!(streams.iter().find(|s| s["codec_type"] == "audio").unwrap()["codec_name"], "aac");
    assert_eq!(p["format"]["format_name"].as_str().map(|f| f.contains("mp4")), Some(true));
    assert_eq!(done.outputs.len(), 1, "posted back to check before it's sent");
    assert_eq!(done.outputs[0].path, out.display().to_string());
    // what it made is a file tiktok.upload takes, from where it is
    let sent = node.run("tiktok.upload", json!({"file": out, "confirm": true})).unwrap().result;
    assert_eq!(sent["status"], "SEND_TO_USER_INBOX");

    // a limit given, cropped, and a video with no sound
    let quiet = test_video(dir.path(), "quiet.mp4", 3, false);
    let r = node
        .run_timeout(
            "tiktok.prepare",
            json!({"file": quiet, "max_seconds": 10, "fit": "crop"}),
            Duration::from_secs(120),
        )
        .unwrap()
        .result;
    assert_eq!((r["width"].as_i64(), r["height"].as_i64()), (Some(1080), Some(1920)));
    assert!(r.get("trimmed").is_none(), "not trimmed (false is left out): {r}");
    assert!(r["text"].as_str().unwrap().contains("cropped to 9:16"), "{r}");

    // with no account to ask, the limit every account has
    let bare = node_with(&fake, dir.path(), json!({}), vec![]);
    let r = bare.run_timeout("tiktok.prepare", json!({"file": quiet}), Duration::from_secs(120)).unwrap().result;
    assert!(r["text"].as_str().unwrap().contains("H.264/AAC"), "{r}");
    let e = bare.run("tiktok.prepare", json!({"file": dir.path().join("nope.mp4")})).unwrap_err();
    assert!(e.message.contains("isn't a file"), "{e}");
    let junk = dir.path().join("junk.mp4");
    std::fs::write(&junk, b"not a video").unwrap();
    let e = bare.run("tiktok.prepare", json!({"file": junk})).unwrap_err();
    assert!(e.message.contains("ffprobe can't read"), "{e}");
}

#[test]
fn files_come_from_the_folders_the_node_allows() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (outside, _) = video_file(elsewhere.path(), "secret.mp4", 1000);
    let node = node(&fake, dir.path());
    let e = node.run("tiktok.upload", json!({"file": outside, "confirm": true})).unwrap_err();
    assert!(e.message.contains("outside the folders"), "{e}");
    let e = node.run("tiktok.upload", json!({"file": "pointman://asset/12", "confirm": true})).unwrap_err();
    assert!(e.message.contains("wasn't fetched by this node"), "{e}");
    let e = node.run("tiktok.prepare", json!({"file": outside})).unwrap_err();
    assert!(e.message.contains("outside the folders"), "{e}");
    assert!(fake.requests().is_empty());
}
