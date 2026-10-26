# tiktok

A Pointman extension that shares videos to TikTok: a YouTube Short as it is, or a cut made for TikTok. Its process
makes a video ready (H.264/AAC, 1080x1920, no longer than the account may post) and sends it through TikTok's
[Content Posting API](https://developers.tiktok.com/doc/content-posting-api-get-started), as a draft in the
account's TikTok inbox or as a direct post, only once the account's owner has said yes. Its behaviour (`SKILL.md`)
tells agents how to caption it, what to ask the owner, and which mode to use.

| File | What |
|---|---|
| `SKILL.md` | The workflow: prepare, caption, ask the owner, send, check; the commands | 
| `proto/tiktok/v1/tiktok.proto` | The commands' params and results, and the settings |

## Install

```
pointman add jamescalam/pointman-tiktok
```

Every agent on the node gets `SKILL.md` in its prompt from its next wake. The commands run on the machines it's
installed on, which need `ffmpeg` (and `ffprobe`) for `tiktok.prepare`.

## The commands

| Command | What it does |
|---|---|
| `tiktok.creator {}` | The signed-in account: its name, who it can post for, its longest video, the interactions it turns off |
| `tiktok.prepare {file, max_seconds?, fit?}` | A new MP4 made with ffmpeg: H.264/AAC, 1080x1920 (padded, or cropped with `fit: crop`), trimmed to the account's longest (or `max_seconds`). Posted back, so the owner sees what goes up |
| `tiktok.upload {file, caption, mode?, privacy?, …, confirm}` | Sends a video as an inbox draft (the default) or a direct post, in TikTok's chunks, with progress. Refused without `confirm: true` |
| `tiktok.status {publish_id}` | Whether TikTok is still taking it, has it in the inbox, posted it, or failed it, and why |
| `/tiktok add`, `/tiktok client` | Open the forms for an account's sign-in, and for the TikTok app's client key and secret, in the app |

**Drafts** go to the account's TikTok inbox (`/v2/post/publish/inbox/video/init/`, scope `video.upload`). The owner
opens the notification in the TikTok app, adds sounds and the caption, and posts. TikTok's inbox takes no caption, so
the answer gives it to paste. At most 5 drafts can wait in 24 hours.

**Direct posts** (`/v2/post/publish/video/init/`, scope `video.publish`) ask TikTok for the creator's info first, as
TikTok requires, and keep to it: the privacy must be one the account has now (and is never a default), interactions
the account turns off can't be allowed, comments, duets and stitches stay off unless asked for, and a video longer
than the account may post is refused. **Until TikTok audits the app,** a direct post goes up `self_only` and only to a
private account: the extension refuses any other privacy while the `audited` setting is off.

**Uploads resume.** A video goes up in chunks (`chunk_mib`, 10 MiB by default; TikTok takes 5 to 64 MiB, the bytes
left over in the last). A dropped connection sends the same chunk again. The upload is the job's checkpoint, so a
node that restarts mid-upload carries on, while TikTok keeps the address (an hour).

## Signing in

TikTok has no device flow, so the sign-in is Login Kit for Desktop's authorization code with PKCE, its redirect
address pasted back:

1. `/tiktok add` in the Pointman app, and tap **Sign in**. TikTok's page opens on any device.
2. Sign in as the TikTok account to post to, and allow it.
3. TikTok sends the browser to `http://127.0.0.1:8370/callback/?code=…&state=…`, which won't load: nothing listens
   there. Copy that whole address from the address bar and paste it into the form.
4. The extension checks the address's `state` is this sign-in's (another is refused), trades the code with its
   verifier, and the refresh token is sealed in the vault as `tiktok.<label>`.

Access tokens last a day and are made again 5 minutes before they run out, or when TikTok refuses one. Refresh tokens
last a year. TikTok may hand back a new refresh token on a refresh; an extension can't write the vault, so the new
one is used from then on in memory, and if TikTok stops taking the vault's after a restart, the answer asks for
`/tiktok add` again.

## Setting it up: your TikTok app

The extension goes through a TikTok app of your own, at [TikTok for Developers](https://developers.tiktok.com/):

1. **Make the app.** Sign in at developers.tiktok.com, then **Manage apps › Connect an app**. Its name can't mention
   TikTok or other social networks.
2. **Add the products:** **Login Kit** and **Content Posting API**. In the Content Posting API's settings, turn on
   **Direct Post** (for `mode: direct`; drafts don't need it).
3. **Add the scopes:** `user.info.basic`, `video.upload` and `video.publish`.
4. **Register the redirect address** in Login Kit's settings (Desktop): `http://127.0.0.1:8370/callback/`, exactly. To
   use another, change the extension's `redirect` setting to match. If the portal asks for platforms, choose Desktop.
5. **Make a sandbox** (the switch next to the app's name › **Create Sandbox**), with the same products and scopes,
   and add your own TikTok account under **Sandbox settings › Target users** (up to 10; it can take an hour to show).
   A sandbox needs no review, so this is how it's used day to day.
6. **In the Pointman app:** `/tiktok client`, and paste the sandbox's (or the app's) client key, a space, then its
   client secret. It's sealed in the vault as `tiktok.client`. Then `/tiktok add` and sign in, as above.
7. **Try a draft first:** `tiktok.upload` with `mode: draft` lands in the account's TikTok inbox.

**Review and audit, for more than drafts and private posts.** A production app needs TikTok's app review: a
working website with its privacy policy and terms visible, a demo video of the whole flow (made in the sandbox), and
an explanation of each product and scope. TikTok doesn't approve apps "for private or personal use", or ones still in
testing. Passing the review and then the Content Posting API's audit lifts the private-only rule for direct posts;
then turn the `audited` setting on.

Settings, per machine: `account` (which sign-in to use, by label; the first if empty), `redirect`, `direct_post`
(ask for `video.publish` at sign-in; off for drafts only), `audited`, and `chunk_mib`.

## Building and testing

```
cargo test
```

The tests run the extension under the SDK's stand-in node against a fake TikTok on a local port (`tests/common`):
the paste-back sign-in (a verifier the fake checks against the challenge, another sign-in's state refused), tokens
made again before they run out, when refused and when TikTok rotates them, a chunked draft upload with a dropped
connection, one resumed from its checkpoint, direct posts kept to the creator's options, refusals without `confirm`,
`tiktok.status`, and `tiktok.prepare` on real videos made with ffmpeg (which the tests need). No real account is
called.

`extension.toml`'s params and settings schemas are written from `proto/tiktok/v1/tiktok.proto` by the SDK's
`pointman-extension manifest write`: change the proto, then write them again. `tests/manifest.rs` checks they match
it field for field. A release is a tag that's the manifest's version (`git tag v0.1.0 && git push origin v0.1.0`):
`.github/workflows/release.yml` builds the four platforms and attaches them to the release.

## Sources

TikTok for Developers: [Login Kit for Desktop](https://developers.tiktok.com/doc/login-kit-desktop),
[access tokens](https://developers.tiktok.com/doc/oauth-user-access-token-management),
[upload (drafts)](https://developers.tiktok.com/doc/content-posting-api-reference-upload-video),
[direct post](https://developers.tiktok.com/doc/content-posting-api-reference-direct-post),
[creator info](https://developers.tiktok.com/doc/content-posting-api-reference-query-creator-info),
[post status](https://developers.tiktok.com/doc/content-posting-api-reference-get-video-status),
[media transfer](https://developers.tiktok.com/doc/content-posting-api-media-transfer-guide),
[content sharing guidelines](https://developers.tiktok.com/doc/content-sharing-guidelines),
[sandbox](https://developers.tiktok.com/doc/add-a-sandbox) and
[app review](https://developers.tiktok.com/doc/app-review-guidelines), read 26 Oct 2026.

MIT licence.
