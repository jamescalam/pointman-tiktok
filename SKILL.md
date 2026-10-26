---
name: tiktok
description: How to share a video to TikTok on Pointman (a YouTube Short as it is, or a cut made for TikTok) with the tiktok commands: make it ready, write its caption, ask the owner, send it as an inbox draft or a direct post, and check where it is. Use it when a task is any of those.
---

# TikTok sharing

For tasks that put a video on a TikTok account: a YouTube Short shared to TikTok, or a cut made for it. Skip it
otherwise.

**The commands,** on the machine with the account signed in: `create_job` (or `/tiktok <verb>` in a thread). `file`
is a board file (`pointman://asset/<id>`) or a path on that machine.

| For | Command |
|---|---|
| The account: its name, who it can post for, its longest video, interactions it turns off | `tiktok.creator {}` |
| A copy made ready: H.264/AAC MP4, 1080x1920, trimmed to the account's longest (posted back to check) | `tiktok.prepare {file, max_seconds?, fit?: pad\|crop}` |
| Send it: to the TikTok inbox as a draft (default), or a direct post | `tiktok.upload {file, caption, mode?: draft\|direct, privacy?, allow_comments?, allow_duets?, allow_stitches?, cover_ms?, your_brand?, branded_content?, ai_generated?, confirm}` |
| Where a sent video is | `tiktok.status {publish_id}` |

**The order.**
1. **Start from the final Short.** Use the exported file, not the timeline. Run `tiktok.prepare` unless it's an
   MP4 at 1080x1920 already, no longer than `tiktok.creator`'s `max_seconds`. It posts the new file back, so the owner
   sees exactly what goes up.
2. **Write the caption from the Short's title and description.** Lead with the hook in plain words, then 3–5
   hashtags (the topic, the tool, the niche: `#claudecode #ai #coding`). **No links:** TikTok strips them, and
   `tiktok.upload` refuses a caption with one. Say "link in bio" instead, if the Short points at a longer video. At
   most 2,200 characters; the first ~100 show before "more".
3. **Ask the owner first, every time.** Post exactly what will go up as a decision: the file (its link), the
   caption, the mode, and for a direct post its privacy and the interactions you'd allow. Add TikTok's consent line
   for a direct post: "By posting, you agree to TikTok's Music Usage Confirmation." (With branded content: "…TikTok's
   Branded Content Policy and Music Usage Confirmation.") Pass `confirm: true` only after their yes, for what they
   said yes to. Never set it on your own.
4. **Default to a draft** (`mode: draft`). It lands in the account's TikTok inbox; the owner opens the notification
   in the app, adds sounds and the caption (TikTok's inbox takes none, so the answer gives it to paste), and posts.
   At most 5 drafts can wait in the inbox in 24 hours.
5. **Direct posts only when the owner asks for one.** `privacy` is theirs to pick (never a default): one of
   `tiktok.creator`'s `privacy_options`. Comments, duets and stitches stay off unless they say; one the account turns
   off can't be allowed. Mark `your_brand`/`branded_content` for promotions, and `ai_generated` for AI-made video.
6. **Report from the answer:** its `publish_id` and status. TikTok takes a few minutes before a direct post is on the
   profile: check with `tiktok.status`.

**Until TikTok audits the app** (the `audited` setting is off): direct posts go up `self_only`, seen only by the
owner, and only while the account is set to private in the TikTok app; anything else is refused. Say so when you
offer a direct post, and suggest a draft instead. Drafts aren't held back.

- `confirm` refusals, privacy refusals and caption refusals happen before anything reaches TikTok: fix and ask again.
- Not signed in? Ask the owner to run `/tiktok add` in the app (and `/tiktok client` first, for the TikTok app's key
  and secret). Never ask for, paste or post a token. Don't retry a failed upload in a loop: TikTok caps uploads a
  minute and a day.

**Stop and ask** before sharing anything that isn't the owner's own video, or with music they may not have the rights
to on TikTok.
