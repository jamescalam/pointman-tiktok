//! The commands' params are proto/tiktok/v1/tiktok.proto's messages (`v1::UploadParams`…), which the SDK's typed
//! handlers read: an unknown field, a missing required one or a word an enum doesn't have fails the command before it
//! runs. What a JSON Schema can't say (a caption's length as TikTok counts it, no links in it), and the limits again
//! for a job that didn't go through the manifest's schema, are checked here.

use anyhow::bail;
use buffa::{EnumValue, Enumeration};
use regex::Regex;

use crate::v1::{Mode, Privacy};

/// A known value of an enum field: None for one left out, unspecified, or a number this build doesn't know.
pub fn known<E: Enumeration + Copy>(v: &EnumValue<E>) -> Option<E> {
    v.as_known().filter(|e| e.to_i32() != 0)
}

impl Privacy {
    /// TikTok's own word for it: `SELF_ONLY`.
    pub fn tiktok(self) -> &'static str {
        self.proto_name().trim_start_matches("PRIVACY_")
    }

    /// The manifest's word for it: `self_only`.
    pub fn word(self) -> String {
        self.tiktok().to_lowercase()
    }

    pub fn from_tiktok(s: &str) -> Option<Self> {
        Privacy::from_proto_name(&format!("PRIVACY_{s}")).filter(|p| p.to_i32() != 0)
    }
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::MODE_DIRECT => "direct",
            Mode::MODE_DRAFT | Mode::MODE_UNSPECIFIED => "draft",
        }
    }
}

/// A caption's length as TikTok counts it: UTF-16 code units.
pub fn caption_length(caption: &str) -> usize {
    caption.encode_utf16().count()
}

pub fn check_caption(caption: &str) -> anyhow::Result<()> {
    let n = caption_length(caption);
    if n > 2200 {
        bail!("caption: at most 2,200 characters as TikTok counts them (UTF-16), not {n}");
    }
    let link = Regex::new(r"(?i)\b(https?://|www\.)\S+").expect("compiles");
    if let Some(m) = link.find(caption) {
        bail!("caption: TikTok strips links, so {} would vanish: take it out (put it in the bio instead)", m.as_str());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::UploadParams;
    use pointman_extension::typed::from_value;

    #[test]
    fn privacy_is_tiktoks_word() {
        assert_eq!(Privacy::PRIVACY_SELF_ONLY.tiktok(), "SELF_ONLY");
        assert_eq!(Privacy::PRIVACY_PUBLIC_TO_EVERYONE.word(), "public_to_everyone");
        assert_eq!(Privacy::from_tiktok("MUTUAL_FOLLOW_FRIENDS"), Some(Privacy::PRIVACY_MUTUAL_FOLLOW_FRIENDS));
        assert_eq!(Privacy::from_tiktok("UNSPECIFIED"), None);
        assert_eq!(Privacy::from_tiktok("SOMETHING_NEW"), None);
        let read: UploadParams =
            from_value(serde_json::json!({"file": "a.mp4", "mode": "direct", "privacy": "self_only"})).unwrap();
        assert_eq!(known(&read.mode), Some(Mode::MODE_DIRECT));
        assert_eq!(known(&read.privacy), Some(Privacy::PRIVACY_SELF_ONLY));
    }

    #[test]
    fn captions() {
        assert!(check_caption("Claude Code in 60 seconds #ai #coding").is_ok());
        assert!(check_caption(&"x".repeat(2200)).is_ok());
        assert!(check_caption(&"😀".repeat(1101)).is_err(), "an emoji is two");
        let e = check_caption("Full video: https://youtu.be/abc #ai").unwrap_err().to_string();
        assert!(e.contains("https://youtu.be/abc") && e.contains("strips links"), "{e}");
        assert!(check_caption("see www.example.com").is_err());
    }
}
