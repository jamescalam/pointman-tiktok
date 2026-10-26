//! What goes up to TikTok as bytes: a video by FILE_UPLOAD (https://developers.tiktok.com/doc/content-posting-api-media-transfer-guide).
//! An init (the inbox's for a draft, the direct post's for a post) gives a `publish_id` and an `upload_url`; the
//! file goes to that address in order, one PUT a chunk, and TikTok answers 206 until it has them all, then 201.
//!
//! TikTok's rules for the chunks: each at least 5 MB and at most 64 MB, but the last, which takes the bytes left over
//! (up to 128 MB); a file smaller than a chunk goes in one; at most 1,000 chunks; a video at most 4 GB. A dropped
//! connection or TikTok's own trouble sends the same chunk again, waiting longer after each failure in a row. The
//! upload is the command's checkpoint, so a node that restarts mid-upload carries on, while TikTok keeps the address
//! (an hour).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use pointman_extension::Job;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tiktok::{failure, Api, Failure};
use crate::words;

pub const MIB: u64 = 1024 * 1024;
/// The biggest video TikTok takes.
pub const MAX_SIZE: u64 = 4 * 1024 * MIB;
/// Failures in a row before an upload gives up.
const TRIES: u32 = 6;
/// How long TikTok keeps an upload address.
const ADDRESS_LASTS: u64 = 60 * 60;

/// How a file is cut into chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub size: u64,
    pub chunk_size: u64,
    pub count: u64,
}

impl Plan {
    /// TikTok's way: chunks of `chunk` (5 to 64 MiB), the bytes left over in the last; one for a file smaller.
    pub fn new(size: u64, chunk: u64) -> anyhow::Result<Plan> {
        if size == 0 {
            bail!("the video is empty");
        }
        if size > MAX_SIZE {
            bail!("the video is {}: TikTok takes at most 4 GB", words::size(size));
        }
        let chunk = chunk.clamp(5 * MIB, 64 * MIB);
        if size < chunk {
            return Ok(Plan { size, chunk_size: size, count: 1 });
        }
        Ok(Plan { size, chunk_size: chunk, count: size / chunk })
    }

    /// Bytes `from..to` of chunk `i`.
    pub fn span(&self, i: u64) -> (u64, u64) {
        let from = i * self.chunk_size;
        let to = if i + 1 == self.count { self.size } else { from + self.chunk_size };
        (from, to)
    }

    /// The chunk that starts at byte `at`, if one does.
    pub fn starting_at(&self, at: u64) -> Option<u64> {
        (0..self.count).find(|i| self.span(*i).0 == at)
    }
}

/// An upload under way: the command's checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub publish_id: String,
    pub upload_url: String,
    /// Which init made it: `draft` or `direct`.
    pub mode: String,
    pub file: String,
    pub modified: u64,
    pub plan: Plan,
    /// The chunks TikTok has.
    pub sent: u64,
    /// When TikTok gave the address (Unix seconds).
    pub at: u64,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The type TikTok takes for a file: MP4, MOV or WebM.
pub fn content_type(path: &Path) -> anyhow::Result<&'static str> {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp4" | "m4v") => Ok("video/mp4"),
        Some("mov") => Ok("video/quicktime"),
        Some("webm") => Ok("video/webm"),
        _ => {
            bail!("{} isn't an MP4, MOV or WebM, the videos TikTok takes: tiktok.prepare makes an MP4", path.display())
        }
    }
}

/// The init's `source_info`, for this plan.
pub fn source_info(plan: &Plan) -> Value {
    json!({"source": "FILE_UPLOAD", "video_size": plan.size, "chunk_size": plan.chunk_size,
           "total_chunk_count": plan.count})
}

/// Start an upload: the inbox's init for a draft (no `post_info`: TikTok's inbox takes none), the direct post's for
/// a post. Its publish id and address.
pub fn init(api: &Api, direct: Option<&Value>, plan: &Plan) -> anyhow::Result<(String, String)> {
    let (path, body) = match direct {
        Some(post_info) => {
            ("post/publish/video/init/", json!({"post_info": post_info, "source_info": source_info(plan)}))
        }
        None => ("post/publish/inbox/video/init/", json!({"source_info": source_info(plan)})),
    };
    let data = api.post(path, Some(&body))?;
    let field = |k: &str| data[k].as_str().filter(|s| !s.is_empty()).map(String::from);
    let id = field("publish_id").ok_or_else(|| anyhow!("TikTok gave no publish_id"))?;
    let url = field("upload_url").ok_or_else(|| anyhow!("TikTok gave no upload address"))?;
    if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1")) {
        bail!("TikTok's upload address isn't https: {url}");
    }
    Ok((id, url))
}

/// How far a chunk got.
enum Put {
    /// TikTok has it, and wants more (206).
    More,
    /// TikTok has every byte (201).
    Done,
    /// TikTok has another part than this chunk follows (416): how many bytes, if it said.
    Has(Option<u64>),
}

/// One chunk, from the file.
fn put(agent: &ureq::Agent, s: &Session, kind: &str, file: &mut File, i: u64) -> Result<Put, Failure> {
    let (from, to) = s.plan.span(i);
    let mut buf = vec![0u8; (to - from) as usize];
    file.seek(SeekFrom::Start(from))
        .and_then(|_| file.read_exact(&mut buf))
        .map_err(|e| Failure::Lost(format!("couldn't read the video: {e}")))?;
    let got = agent
        .put(&s.upload_url)
        .set("Content-Type", kind)
        .set("Content-Range", &format!("bytes {from}-{}/{}", to - 1, s.plan.size))
        .send_bytes(&buf);
    match got {
        Ok(r) if r.status() == 201 || i + 1 == s.plan.count => Ok(Put::Done),
        Ok(_) => Ok(Put::More),
        Err(ureq::Error::Status(416, r)) => Ok(Put::Has(r.header("Content-Range").and_then(has))),
        Err(e) => Err(failure(e)),
    }
}

/// How many bytes TikTok says it has, from an answer's `Content-Range: bytes 0-<last>/<size>`.
pub fn has(range: &str) -> Option<u64> {
    let span = range.trim().strip_prefix("bytes")?.trim().split('/').next()?;
    let last: u64 = span.split('-').nth(1)?.trim().parse().ok()?;
    Some(last + 1)
}

/// Whether to send a chunk again: no answer, or TikTok's own trouble.
fn worth_again(f: &Failure) -> bool {
    match f {
        Failure::Lost(_) => true,
        Failure::Said { status, .. } => *status >= 500,
    }
}

fn wait(job: &Job, failures: u32) -> anyhow::Result<()> {
    let total = Duration::from_secs((1u64 << failures.saturating_sub(1).min(5)).min(30));
    let mut waited = Duration::ZERO;
    while waited < total {
        job.check()?;
        std::thread::sleep(Duration::from_millis(100));
        waited += Duration::from_millis(100);
    }
    Ok(())
}

/// Whether a checkpoint is this file's upload, and TikTok still keeps its address.
pub fn still(s: &Session, path: &Path, plan: &Plan, mode: &str) -> bool {
    s.file == path.display().to_string()
        && s.modified == modified(path)
        && s.plan == *plan
        && s.mode == mode
        && now() < s.at + ADDRESS_LASTS - 60
}

/// Send the file's chunks, from the session's `sent`, saving the checkpoint after each.
pub fn chunks(job: &Job, agent: &ureq::Agent, path: &Path, mut s: Session) -> anyhow::Result<Session> {
    let kind = content_type(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut file = File::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
    let mut failures = 0;
    while s.sent < s.plan.count {
        job.check()?;
        let (from, _) = s.plan.span(s.sent);
        job.progress(
            from as f64 / s.plan.size as f64,
            format!("uploading {name}: {} of {}", words::size(from), words::size(s.plan.size)).as_str(),
        );
        let lost = match put(agent, &s, kind, &mut file, s.sent) {
            Ok(Put::Done) => {
                s.sent = s.plan.count;
                job.save(&s);
                break;
            }
            Ok(Put::More) => {
                s.sent += 1;
                failures = 0;
                job.save(&s);
                continue;
            }
            Ok(Put::Has(n)) => {
                // TikTok has another part than the checkpoint says: carry on from where it is, at a chunk's start
                match n.and_then(|n| s.plan.starting_at(n)) {
                    Some(i) if failures < TRIES => {
                        failures += 1;
                        s.sent = i;
                        continue;
                    }
                    _ => bail!("TikTok's upload of {name} is out of step with this one (416): send it again"),
                }
            }
            Err(f) => f,
        };
        match &lost {
            Failure::Said { status: 403, .. } => {
                bail!("TikTok's upload address for {name} has run out (it lasts an hour): send it again")
            }
            Failure::Said { status: 404, .. } => bail!("TikTok no longer has this upload ({lost}): send it again"),
            f if !worth_again(f) => return Err(lost.into()),
            _ => {}
        }
        failures += 1;
        if failures >= TRIES {
            bail!("gave up uploading {name} after {TRIES} failures in a row at {}: {lost}", words::size(from));
        }
        job.handle().log(format!("{name}: {lost}; sending chunk {} again (failure {failures} in a row)", s.sent + 1));
        wait(job, failures)?;
    }
    job.progress(1.0, format!("uploaded {name}").as_str());
    Ok(s)
}

/// Where a publish is, as status/fetch says: its `data`.
pub fn status(api: &Api, publish_id: &str) -> anyhow::Result<Value> {
    api.post("post/publish/status/fetch/", Some(&json!({"publish_id": publish_id})))
}

/// Ask for the status until it's past processing the upload, or `patience` is up: the last answer.
pub fn settle(job: &Job, api: &Api, publish_id: &str, patience: Duration, every: Duration) -> anyhow::Result<Value> {
    let end = std::time::Instant::now() + patience;
    loop {
        let v = status(api, publish_id)?;
        let st = v["status"].as_str().unwrap_or_default();
        if st != "PROCESSING_UPLOAD" || std::time::Instant::now() + every > end {
            return Ok(v);
        }
        job.progress(None, "TikTok is processing it");
        let mut waited = Duration::ZERO;
        while waited < every {
            job.check()?;
            std::thread::sleep(Duration::from_millis(100));
            waited += Duration::from_millis(100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_tiktoks() {
        let p = Plan::new(50_000_123, 10 * MIB).unwrap();
        assert_eq!((p.chunk_size, p.count), (10 * MIB, 4), "the rest goes in the last");
        assert_eq!(p.span(3), (30 * MIB, 50_000_123));
        assert_eq!(Plan::new(3 * MIB, 10 * MIB).unwrap(), Plan { size: 3 * MIB, chunk_size: 3 * MIB, count: 1 });
        assert_eq!(Plan::new(20 * MIB, MIB).unwrap().chunk_size, 5 * MIB, "at least 5 MiB");
        assert_eq!(Plan::new(200 * MIB, 100 * MIB).unwrap().chunk_size, 64 * MIB, "at most 64 MiB");
        let last = Plan::new(128 * MIB - 1, 64 * MIB).unwrap();
        assert_eq!((last.count, last.span(0)), (1, (0, 128 * MIB - 1)), "a last chunk up to 128 MiB");
        assert!(Plan::new(0, 10 * MIB).is_err());
        assert!(Plan::new(MAX_SIZE + 1, 10 * MIB).is_err());
        assert_eq!(p.starting_at(20 * MIB), Some(2));
        assert_eq!(p.starting_at(20 * MIB + 1), None);
    }

    #[test]
    fn it_reads_how_much_tiktok_has() {
        assert_eq!(has("bytes 0-10485759/50000123"), Some(10 * MIB));
        assert_eq!(has("bytes */50000123"), None);
        assert_eq!(content_type(Path::new("a/final.MP4")).unwrap(), "video/mp4");
        assert!(content_type(Path::new("cut.mkv")).is_err());
    }
}
