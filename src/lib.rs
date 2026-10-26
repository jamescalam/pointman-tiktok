//! TikTok on Pointman: videos sent to a TikTok account through the Content Posting API, as drafts in its inbox or
//! direct posts (`upload`), made ready with ffmpeg first (`prepare`), the account signed in from the apps with Login
//! Kit (`signin`), and the calls to TikTok (`tiktok`). The commands' params are proto/tiktok/v1/tiktok.proto's
//! (`params`).

pub mod params;
pub mod prepare;
pub mod signin;
pub mod tiktok;
pub mod upload;
pub mod words;

/// proto/tiktok/v1/tiktok.proto's messages, as buffa builds them (build.rs).
pub mod proto {
    pointman_extension::include_proto!();
}
pub use proto::tiktok::v1;

/// The vault item with the TikTok app's client key and secret: every other `tiktok.<label>` is an account's sign-in.
pub const CLIENT_ITEM: &str = "tiktok.client";
