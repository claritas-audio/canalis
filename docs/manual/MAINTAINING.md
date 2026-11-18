# Maintaining the manual and the docs site

The manual in this folder is the only source of the Canalis docs. The website
([claritas.meltingbots.com/canalis/docs](https://claritas.meltingbots.com/canalis/docs),
repo `claritas-audio/claritas-site`) shows the same pages; it copies them, it never edits them.

## How the pages are laid out
- One file per page. `README.md` lists them in order as a numbered list of links
  (`1. [Title](slug.md): intro`); the site reads that list for the order and the sidebar, so
  a page that is not in it is not on the site.
- The manual mimics the site on GitHub: the contents list carries each page's intro (the
  site's overview cards) and every page ends with "← Previous · Contents · Next →" (the site's
  footer). Both are generated: after adding, removing, renaming or reordering a page, or
  changing an intro, run `python3 scripts/manual.py` (`--check` only reports). Never edit
  below the `<!-- nav: ... -->` line by hand; the site drops it.
- Every page starts with the shared comment line, then `# Title`, then **one sentence** that
  says what the page covers. The site uses that sentence as the page description (search
  results, link previews, the docs index cards), so keep it a single sentence ending in a
  full stop.
- Sections are `##` (they make the page outline on the site), sub-sections `###`.
- Link to another page as `slug.md` or `slug.md#heading-anchor` (GitHub's anchors, which the
  site uses too). Screenshots are `../screenshots/<file>.png`; the site copies the ones a page
  uses.
- Tips and notes are GitHub callouts, at most two per page:
  ```
  > [!TIP]
  > Raise the send slowly, or listen on headphones.
  ```
  (`[!NOTE]` likewise.) GitHub draws them as boxes; the site sync turns them into its yellow
  callouts.
- Otherwise plain Markdown: no HTML, no MDX, so GitHub and the site render the same thing.

## When something changes
- **A feature users see:** update the page it belongs to in the same commit as the code
  (this replaces the old rule "update docs/manual/canalis.md"). A new view or a big feature
  gets its own page: add the file and a line in `README.md`.
- **A new channel or a changed install command:** update
  [getting-started.md](getting-started.md) › Install here, the README's Install, and in the
  site repo the download cards in `src/pages/canalis.astro` (a channel goes live by getting an
  `href`).
- **New screenshots:** replace the files in `docs/screenshots/` (how they are made is in
  `docs/HANDOFF.md`, 2026-11-16: a private demo PipeWire, 1440×900). The tour GIF is a GitHub
  attachment on issue #77, not a file here.

## At a release, channel by channel
1. **GitHub Releases:** bump `version` in `Cargo.toml`, add a `<release>` to the metainfo,
   tag `v<version>` and push the tag; the release workflow attaches the `.rpm`, the `.deb`,
   the tarball and `SHA256SUMS` (`docs/DEVELOPMENT.md` › Releases).
2. **Install script:** nothing; it asks GitHub for the latest release. If `install.sh`
   itself changed, `pnpm docs:sync` in the site repo copies it to `/install.sh`.
3. **COPR:** `copr-cli buildscm … --commit v<version>` (`docs/DEVELOPMENT.md` › COPR).
4. **AUR** (on hold until there is an account): `packaging/aur/update.sh <version>`, commit,
   then push `PKGBUILD` and `.SRCINFO` to the AUR (`docs/DEVELOPMENT.md` › AUR).
5. **Flathub** (once it is there): a PR to `flathub/io.github.claritas_audio.Canalis` with
   the new tag and commit and regenerated Cargo sources (`packaging/flatpak/SUBMITTING.md`).
6. **Site:** nothing for the version (the cards link to the latest release); `pnpm docs:sync`
   if the manual changed since the last sync.

## Publishing to the site
In the `claritas-site` checkout, next to this one:
```sh
CANALIS_REPO=../../rust/canalis pnpm docs:sync # copies the pages and their screenshots
pnpm check && pnpm build                    # build also indexes the docs for search (Pagefind)
```
then commit `src/content/canalis-docs/` and `public/docs/canalis/` in the site repo. The copy
is generated: fix a typo here and sync, never in the site repo.
