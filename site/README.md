# Reborn's website

The page at <https://schulzcode.github.io/Y2Reborn/>. Plain HTML, one stylesheet and one script; nothing to
build. `.github/workflows/pages.yml` publishes this folder when it changes on `main`.

- Colours and type are the base skin's (`crates/reborn-ui/src/theme.rs`); the font is Open Sans, a Latin subset
  made with `pyftsubset` (`fonts/OpenSans-OFL.txt`).
- `img/` holds 480×360 pictures from `reborn-preview` (`tools/preview/render_previews.py`), `img/skin-*.png` is
  Now Playing (`14-now-playing`) rendered with `--skin skins/<name>`. `img/videos.png` and `img/film.png` are
  captures from a player. Render them again when the UI changes visibly.
- To look at it locally, open `index.html` in a browser.
- "What's new" is made in the browser from the release notes of Y2Linux (`site.js`: the part before the
  `github-only` marker, the sections What's new, Fixed and Changed, each point as its name and first sentence).
  `releases.json` is a copy for a visitor GitHub does not answer; the workflow renews it at every publish, and
  `gh api 'repos/SchulzCode/Y2Linux/releases?per_page=8' --jq '[.[] | select(.draft|not) | {name, tag_name,
  published_at, html_url, body}]' > site/releases.json` renews it by hand.
- Motion (`site.js`, last part): things ease in when scrolled to, the tour pins a screen and changes it with
  the scroll position, numbers count up, the wave draws itself. No library. With "reduce motion" set in the
  system, or without scripts, the page is still and complete.
