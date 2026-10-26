//! A video made ready for TikTok with ffmpeg: H.264 and AAC in an MP4 that plays as it downloads, 1080x1920 (9:16,
//! scaled and padded or cropped), 23 to 60 frames a second, and no longer than the creator may post
//! (https://developers.tiktok.com/doc/content-posting-api-media-transfer-guide, "Video restrictions"). A Short
//! that's 1080x1920 already is only re-encoded.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use pointman_extension::Job;
use serde_json::Value;

use crate::v1::Fit;

pub const WIDTH: u32 = 1080;
pub const HEIGHT: u32 = 1920;

/// ffmpeg's tools, found on the PATH or where a package manager puts them (the node's PATH can be short).
pub fn tool(name: &str) -> anyhow::Result<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/run/current-system/sw/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".nix-profile/bin"));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file()).ok_or_else(|| {
        anyhow!(
            "no {name} on this machine: install ffmpeg (brew install ffmpeg, apt install ffmpeg, or nixpkgs' ffmpeg)"
        )
    })
}

/// What ffprobe says of a video.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub seconds: f64,
    pub fps: f64,
    pub video: String,
    pub audio: Option<String>,
}

/// A rate as ffprobe writes it (30000/1001, 25/1, 0/0).
fn rate(s: &str) -> f64 {
    match s.split_once('/') {
        Some((a, b)) => {
            let (a, b) = (a.parse::<f64>().unwrap_or(0.0), b.parse::<f64>().unwrap_or(0.0));
            if b > 0.0 {
                a / b
            } else {
                0.0
            }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

/// ffprobe's JSON (`-show_streams -show_format`) read: the first video stream, and the first audio one.
pub fn read_probe(v: &Value) -> anyhow::Result<Probe> {
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video").ok_or_else(|| anyhow!("it has no video stream"))?;
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    let (mut width, mut height) =
        (video["width"].as_u64().unwrap_or(0) as u32, video["height"].as_u64().unwrap_or(0) as u32);
    // a phone's portrait video is often landscape pixels with a rotation, which ffmpeg applies
    let rotation = video["side_data_list"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|d| d["rotation"].as_f64())
        .or_else(|| video["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
        .unwrap_or(0.0);
    if (rotation.abs() - 90.0).abs() < 1.0 || (rotation.abs() - 270.0).abs() < 1.0 {
        std::mem::swap(&mut width, &mut height);
    }
    let seconds = v["format"]["duration"]
        .as_str()
        .or_else(|| video["duration"].as_str())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    let fps = [video["avg_frame_rate"].as_str(), video["r_frame_rate"].as_str()]
        .into_iter()
        .flatten()
        .map(rate)
        .find(|r| *r > 0.0)
        .unwrap_or(0.0);
    Ok(Probe {
        width,
        height,
        seconds,
        fps,
        video: video["codec_name"].as_str().unwrap_or_default().to_string(),
        audio: audio.and_then(|a| a["codec_name"].as_str()).map(String::from),
    })
}

pub fn probe(ffprobe: &Path, path: &Path) -> anyhow::Result<Probe> {
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("couldn't run {}", ffprobe.display()))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        bail!("ffprobe can't read {}: {}", path.display(), why.trim().lines().last().unwrap_or("no reason given"));
    }
    let v: Value = serde_json::from_slice(&out.stdout).context("ffprobe's answer isn't JSON")?;
    read_probe(&v).with_context(|| format!("{} isn't a video", path.display()))
}

/// The frame rate it's made at: its own, inside TikTok's 23 to 60, or 30.
pub fn frame_rate(fps: f64) -> Option<&'static str> {
    if (23.0..=60.5).contains(&fps) {
        None
    } else if fps > 60.5 {
        Some("60")
    } else {
        Some("30")
    }
}

/// ffmpeg's arguments, from `input` to `out`, at most `max` seconds long.
pub fn args(input: &Path, out: &Path, p: &Probe, max: Option<f64>, fit: Fit) -> Vec<String> {
    let scale = match fit {
        Fit::FIT_CROP => format!(
            "scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=increase,crop={WIDTH}:{HEIGHT}"
        ),
        _ => format!(
            "scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease,pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=black"
        ),
    };
    let mut a: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into(), "-y".into(), "-i".into()];
    a.push(input.display().to_string());
    if let Some(max) = max {
        a.extend(["-t".into(), format!("{max:.3}")]);
    }
    a.extend(["-map", "0:v:0", "-map", "0:a:0?", "-vf"].map(String::from));
    a.push(format!("{scale},setsar=1,format=yuv420p"));
    if let Some(r) = frame_rate(p.fps) {
        a.extend(["-r".into(), r.into()]);
    }
    a.extend(
        [
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "18",
            "-profile:v",
            "high",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-nostats",
        ]
        .map(String::from),
    );
    a.push(out.display().to_string());
    a
}

/// Run ffmpeg, with its progress as the job's, stopped if the job is cancelled.
pub fn run(job: &Job, ffmpeg: &Path, args: &[String], seconds: f64) -> anyhow::Result<()> {
    let mut child = Command::new(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("couldn't run {}", ffmpeg.display()))?;
    let mut err = child.stderr.take().expect("piped");
    let said = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let out = child.stdout.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        if job.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            job.check()?;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                    if seconds > 0.0 {
                        job.progress((us / 1e6 / seconds).clamp(0.0, 1.0), "making it ready for TikTok");
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    let said = said.join().unwrap_or_default();
    if !status.success() {
        let tail: Vec<&str> = said.trim().lines().rev().take(3).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("ffmpeg failed ({status}): {}", tail.join(" / "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn it_reads_ffprobe() {
        let v = json!({"streams": [
            {"codec_type": "video", "codec_name": "hevc", "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001",
             "side_data_list": [{"rotation": -90}]},
            {"codec_type": "audio", "codec_name": "aac"}],
            "format": {"duration": "61.500000"}});
        let p = read_probe(&v).unwrap();
        assert_eq!((p.width, p.height), (1080, 1920), "turned as it plays");
        assert!((p.fps - 29.97).abs() < 0.01 && (p.seconds - 61.5).abs() < 1e-9);
        assert_eq!(p.audio.as_deref(), Some("aac"));
        assert!(read_probe(&json!({"streams": [{"codec_type": "audio"}]})).is_err());
    }

    #[test]
    fn frame_rates_are_kept_inside_tiktoks() {
        assert_eq!(frame_rate(29.97), None);
        assert_eq!(frame_rate(60.0), None);
        assert_eq!(frame_rate(120.0), Some("60"));
        assert_eq!(frame_rate(15.0), Some("30"));
        let p = Probe { fps: 120.0, ..Default::default() };
        let a = args(Path::new("in.mov"), Path::new("out.mp4"), &p, Some(180.0), Fit::FIT_CROP).join(" ");
        assert!(a.contains("-t 180.000") && a.contains("crop=1080:1920") && a.contains("-r 60"), "{a}");
        let a = args(Path::new("in.mov"), Path::new("out.mp4"), &p, None, Fit::FIT_PAD).join(" ");
        assert!(!a.contains("-t ") && a.contains("pad=1080:1920"), "{a}");
    }
}
