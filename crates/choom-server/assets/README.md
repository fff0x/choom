# Vendored web assets

`asciinema-player.min.js.gz` and `asciinema-player.css.gz` are
[asciinema-player](https://github.com/asciinema/asciinema-player) v3.8.0, served
self-hosted for the read-only web view (no third-party CDN). They are embedded
**gzip-compressed** and served with `Content-Encoding: gzip` to keep the binary small.

asciinema-player is licensed under the **Apache License 2.0**, which is compatible
with choom's GPL-3.0-or-later (Apache-2.0 → GPLv3 is permitted). Upstream copyright
and license: https://github.com/asciinema/asciinema-player/blob/master/LICENSE

To update: fetch the `dist/bundle/` files for the desired release, gzip them
deterministically (`gzip -9 -n`), and replace these — then re-check the live WebSocket
format in `../src/web.rs` (`stream_session`).
