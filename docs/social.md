# Mastodon and Bluesky posts

Blocksatz writes the social media posts for an article and hands them to
the blog. Sending is done by the WordPress plugin **WP Social Publisher**
(1.10 or later) on the blog, so the access tokens for Mastodon and Bluesky
never leave WordPress and a scheduled article still goes out on time while
Blocksatz is closed.

## In the app

The "Beitrag" view has a "Social Media" group:

- **Einstieg für automatische Texte** – a line in front of texts the blog
  writes itself.
- **One row per network** with a switch. Switched on, the network goes out
  with the article. The text field may stay empty: the blog then writes the
  text from title, excerpt and tags, as its own dialog in wp-admin would.
  "Vorschlag vom Blog" fills in that text to edit further (needs the
  article on the blog once), "Automatisch" clears it again.
- **Anhang** – featured image, link preview (Bluesky) or none; "Voreinstellung
  des Blogs" leaves it to the plugin's settings.
- The subtitle counts characters against the network's limit as the blog
  reports it (Mastodon counts every link as 23). The blog checks again
  before anything is planned.
- Below the group: what the blog has planned or sent, and for an article
  that is already online, **Jetzt teilen** (asks first; networks already
  shared are skipped).

Nothing is written to the file until a control in the group is touched, so
an article whose posts were planned in wp-admin is left alone by uploads.

## In the file

```yaml
social_networks: ["mastodon", "bluesky"]
social_intro: "Neu im Blog:"
social_mastodon: "Own text …\n\n{link}\n\n#Linux"
social_bluesky_embed: card
```

- `social_networks` – the networks that go out. A text stays in the file
  when its network is switched off.
- `social_<network>` – the text as a JSON string (valid YAML, line breaks
  as `\n`); a plain value written by hand works too. Missing or empty: the
  blog writes it.
- `social_<network>_embed` – `image`, `card` or `none`; missing: the blog's
  default.
- `{link}` stands for the permalink, which often isn't final while writing.

## What happens on upload

Every upload of a post that isn't online yet sends the texts as the REST
field `wpsp_social`:

```json
{ "intro": "Neu im Blog:", "items": [ { "network": "mastodon", "text": "…{link}…", "embed": "" } ] }
```

The plugin replaces `{link}`, checks each text against the network's limit
and stores the plan. WordPress saves REST fields before it fires
`wp_after_insert_post`, so a post that is published by the same request is
shared right away; a scheduled post is shared when WordPress publishes it.
No networks switched on withdraws an earlier plan. The upload's success
message lists what was planned, sent or refused (the plugin reports a
problem as `notice` in the answer instead of failing the request, since the
post itself is already saved).

Once the post is online, uploads no longer send the field, and editing the
texts doesn't count as an unsent change. Sharing then goes through
`POST wpsp/v1/posts/<id>/share`.

## Plugin endpoints used

| Route | Purpose |
|---|---|
| `GET wpsp/v1/status?post=<id>` | Networks (limit, attachments, default, already shared), plan, last result. 404: plugin not active. |
| `POST wpsp/v1/posts/<id>/generate` | Texts as the plugin writes them, with `{link}` |
| `POST wpsp/v1/posts/<id>/share` | Share an online post now |
| field `wpsp_social` on `wp/v2/posts` | Plan with the upload; read back in the answer |

Code: `src/social.rs` (model, frontmatter, counting, requests),
`src/socialpane.rs` (the group), `src/export.rs` (sending with the upload).
The plugin lives in `web/linuxundich/plugins/wp-social-publisher`
(`includes/class-wpsp-rest.php`).
