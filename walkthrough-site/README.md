# Code walkthroughs

A static website that walks readers through real source code. It is a platform plus its walkthroughs: the layout lives in `templates/` and `sass/`, and each walkthrough is one folder of Markdown under `content/`. The first walkthrough is `content/wakey/`, a tour through this repository's firmware.

This file is for authors: how to run the site, add a walkthrough, add a chapter, and use each component. The rules for writing chapters are in [AGENTS.md](AGENTS.md), and the language guide is [LANGUAGE.md](LANGUAGE.md).

## What you need

| Tool | Version | For |
| --- | --- | --- |
| [Zola](https://www.getzola.org/) | **0.23.6** | building the site |
| Python | 3.11 or later | the scripts in `scripts/` (standard library only) |
| Git | any | the scripts read source files at a commit |

Install Zola with `brew install zola` and check the version with `zola --version`. The site needs 0.23.x: that release replaced shortcodes with Tera 2 components, and templates written for it do not build with 0.22 or earlier. When you look something up, use the documentation for 0.23 and for [Tera 2](https://keats.github.io/tera/); older examples use syntax that no longer exists.

## Run it

```sh
cd walkthrough-site
zola serve          # http://127.0.0.1:1111, rebuilt on every change
zola build          # writes the site into public/
scripts/check       # everything that must hold before publishing
```

`scripts/check` runs `zola build` and `zola check`, compares every code excerpt with the source at its commit, makes sure the built pages load nothing from another site, and prints the reading time of each walkthrough. `scripts/check --time 230-290` also fails if a walkthrough's total is outside that range of minutes.

## How the site is put together

```text
zola.toml               configuration
content/                all text: one folder per walkthrough
  _index.md             the landing page
  wakey/_index.md       a walkthrough: its overview page and its settings
  wakey/NN-name.md      a chapter
  wakey/map.svg         the walkthrough's "where we are" map
  wakey/img/            its images and diagrams
templates/              all layout (HTML)
  components.html       what content files can call: callouts, figures, excerpts
sass/                   all styles
static/                 fonts, scripts, Mermaid (served from the site itself)
scripts/                excerpt, verify-excerpts, check
skeleton/               a walkthrough to copy
```

Content files hold Markdown and component calls, and no HTML. If a chapter needs something the components do not offer, add a component; do not write markup into the chapter.

**Any base URL.** The built site uses only relative URLs, so `public/` can be copied to any address or sub-folder without a rebuild. `base_url` in `zola.toml` is a placeholder that the templates replace; leave it as it is.

**No external requests.** Fonts (Archivo, Source Serif 4, JetBrains Mono, all under the SIL Open Font License) and Mermaid (MIT) are in `static/`, with their licence files beside them. `scripts/check` fails if a page loads anything from elsewhere.

## Add a walkthrough

1. Copy `skeleton/my-walkthrough/` to `content/<name>/`. The folder name becomes the address.
2. Edit `content/<name>/_index.md`. Its comments explain every setting. The ones that matter most:
   - `commit`: the commit the walkthrough describes. Every excerpt is read at this commit.
   - `source`: the path from this folder to the Git repository, if the site is not inside it.
   - `repo`: the repository's web address; the file name above each excerpt then links to the file at the commit, and the overview shows a button to the repository.
   - `shots`: one to three screenshots (16:9, in `img/`, each with `src` and `alt`) that the overview fans out beside its title, the first in front.
   - `accent` and `spark`: the walkthrough's two colours. `accent` fills the large blocks and needs to be dark enough for white text. `spark` is the highlight and needs to be light enough for dark text.
   - `map`: the "where we are" map (below).
3. Draw the map, `content/<name>/map.svg`: one `<g data-part="name">` per part of the project, each holding a `<rect class="m-box">` and a `<text class="m-label">`. A chapter lights up parts by naming them in its front matter.
4. Write the chapters.

Nothing outside the new folder needs to change. The landing page lists every folder under `content/` as a card, with its number of chapters and total reading time.

## Add a chapter

A chapter is one Markdown file in the walkthrough's folder. Name it `NN-short-name.md`; the name becomes its address, and `weight` in the front matter sets its place.

```toml
+++
title = "The board: one take, many owners"
description = "One sentence under the title and in the chapter list."
weight = 6

[extra]
map = ["board", "display"]
map_note = "Where this chapter is in the map, in a sentence or two."
goals = [
  "three to five things the reader will understand afterwards",
]
takeaways = [
  "Three to five full sentences.",
]
+++
```

The template builds the chapter's frame from this: the header with number and reading time, the "After this chapter you will understand" box from `goals`, the mini-map with the parts in `map` highlighted, and the "Key takeaways" block from `takeaways`. The Markdown below the front matter is the tour itself. Use `##` and `###` headings; they become the table of contents on the right.

A chapter that is planned but not written can carry `planned = true` under `[extra]`: it is listed without a link and does not count towards the reading time.

**Reading time** is Zola's word count of the file (text, code, goals and takeaways) divided by the walkthrough's `wpm` and rounded up. The overview and the landing page add the chapters up.

**Links** to other chapters use Zola's checked form, `[chapter 5](@/wakey/05-threads.md)`; a wrong name fails the build.

## Components

Zola 0.23 has no shortcodes. Content files call components, defined in `templates/components.html`, in one of two forms: `{{ <name a="…" /> }}` for a component without a body, and `{% <name a="…"> %} … {% </name> %}` for one with a body. Text arguments are quoted; `true`, `false` and numbers are written in braces, as in `wide={true}`.

Tera reads `{{`, `{%` and `{#` anywhere in a content file. Code excerpts are protected; in your own text, avoid those three sequences.

### Callouts

Four kinds, each with an optional `title` and `id`. The body is Markdown.

```text
{% <concept title="What an executor does"> %}
An idea from outside the language that the reader needs here.
{% </concept> %}

{% <tip title="`StaticCell`: a `&'static mut` without `static mut`" id="static-cell"> %}
A Rust idiom, pattern or type-system technique the code uses.
{% </tip> %}

{% <pitfall title="A task cannot interrupt another task" id="cooperative"> %}
Something that goes wrong easily, and what the code does about it.
{% </pitfall> %}

{% <unsafe title="Reborrowing the modem" id="unsafe-modem-reborrow"> %}
An `unsafe` block and exactly what makes it sound.
{% </unsafe> %}
```

Give every tip, pitfall and unsafe callout an `id`. A recap page can then repeat it with `chapter="05-threads" id="…"`, which adds a link back to where the code does it:

```text
{% <tip title="…" id="static-cell" chapter="03-executor"> %}
The short version.
{% </tip> %}
```

### Code excerpts

Never type an excerpt. Let the script paste it from the commit:

```sh
scripts/excerpt wakey firmware/src/runtime.rs 143-157 --hl "147-153"
scripts/excerpt wakey firmware/src/app/mod.rs 164-216 --hl "176" --skip "181-200"
```

The first argument is the walkthrough, then the file (from the repository root) and the lines. `--hl` marks the lines the text discusses; `--skip` leaves lines out. The script prints a `code` component call; paste it into the chapter unchanged:

```text
{% <code file="firmware/src/runtime.rs" lines="143-157" hl="147-153"> %}{% raw %}
pub fn start(init: impl FnOnce(Spawner) + Send + 'static) -> ! {
    …
}
{% endraw %}{% </code> %}
```

On the page this becomes a block with the file name (a link to the commit if the walkthrough has a `repo`), the line range, the file's own line numbers, the highlighted lines and a copy button. Lines left out show as a `// ...` row, and the numbering continues correctly after it.

`scripts/verify-excerpts` compares every excerpt with the file at the commit, character for character, and fails on any difference. It also fails if an excerpt shows more than 32 lines; aim for 30 or fewer. To move a walkthrough to a newer commit, change `commit`, run the script, and regenerate the excerpts it reports.

### Figures

```text
{{ <figure src="img/ui-clock.jpg" alt="The clock face in the morning" caption="The clock page." /> }}

{% <gallery caption="The three settings pages."> %}
{{ <figure src="img/ui-alarm-list.png" alt="…" /> }}
{{ <figure src="img/ui-alarm-page.png" alt="…" /> }}
{% </gallery> %}
```

`src` is relative to the walkthrough's folder. `alt` is required. `width="narrow"` limits a single image's width. Images go into the walkthrough's `img/` folder, which needs the small `_index.md` that the skeleton has (it makes Zola copy the folder).

For this repository, the UI pictures come from `tools/ui-preview` (`cargo run --release -- <output folder>`), which renders the firmware's pages on the host.

### Diagrams

A Mermaid diagram is written in the chapter and drawn in the reader's browser in the site's colours, light or dark:

```text
{% <diagram caption="Start-up in order."> %}
sequenceDiagram
    participant A as app
    participant W as wifi
    A->>W: net::start
{% </diagram> %}
```

Without scripts the reader sees the source, which stays readable. Mermaid is loaded only on pages that have a diagram.

A hand-written SVG is a file in `img/` that the component puts into the page itself, so that it takes the site's colours:

```text
{{ <svg src="img/executor.svg" caption="The executor's loop." /> }}
```

A diagram never shrinks until its text is unreadable: below a least width it scrolls sideways inside its frame. Mermaid diagrams work that width out themselves; a large SVG can ask for more room with `wide={true}`. An SVG file sets no colours of its own; it uses these classes (defined in `sass/_blocks.scss`):

| Class | For |
| --- | --- |
| `d-box`, `d-box--soft`, `d-box--accent`, `d-box--spark`, `d-box--dashed` | boxes: plain, quiet, in the accent colour, highlighted, outline only |
| `d-line`, `d-line--spark`, `d-line--dashed` | lines and arrows |
| `d-head`, `d-head--spark` | the path inside a `<marker>` (arrowheads) |
| `d-text`, `d-text--title`, `d-text--small`, `d-text--mono`, `d-text--on-accent`, `d-text--spark` | text |
| `m-box`, `m-label` | the mini-map only |

Two rules for SVG files: no blank lines (one would cut the figure in two), and every `<marker>` id unique across the site, because all SVGs of a page share one document. `content/wakey/img/executor.svg` is a model.

## Design

The site's own words are in `zola.toml`: `title` and `description` are the landing page's heading and the line below it, and `[extra]` holds `brand` (the name in the top bar's left corner, where the site is deployed), `eyebrow` (the small line above the heading) and `github` (the author's profile, a button on the landing page). The picture in the top bar is `static/img/avatar.jpg`, the author's GitHub picture; `static/favicon-32.png` and `static/apple-touch-icon.png` are made from it (with ImageMagick: `magick avatar -resize 32x32 favicon-32.png`, and 180x180 for the touch icon), so replace all three together.

The look is set by custom properties in `sass/_theme.scss`: the paper, ink and rule colours for light and dark, the callout colours, and the three type families. A walkthrough only brings its `accent` and `spark`. The text column is limited to about 70 characters (`--measure`); code, figures and tables may use the full column.

The theme follows the system and can be switched with the button in the top bar; the choice is remembered in the browser. Adding `?theme=dark` or `?theme=light` to an address forces one, which is useful for screenshots.

## Deploy

```sh
./deploy.sh --dry-run   # what would be uploaded
./deploy.sh             # check, build, upload
```

`deploy.sh` runs `scripts/check`, then copies `public/` with rsync over SSH to the host `wackazong` (an entry in `~/.ssh/config`), folder `/wackazong`, the document root of wackazong.com. It deletes nothing on the server: files that are not part of the site stay, and a page removed from the site stays online until it is deleted by hand. `DEPLOY_HOST` and `DEPLOY_DIR` change the target.

## Search

`zola build` writes a search index (`search_index.en.json`). The search box opens with `/` or Ctrl/Cmd+K, searches titles and text of all walkthroughs, and names the walkthrough and chapter of each result. `?q=words` in an address opens it with a query.

## Files worth knowing

| File | What it is |
| --- | --- |
| `AGENTS.md` | rules for writing chapters, for humans and AI assistants |
| `LANGUAGE.md` | the language guide |
| `OUTLINE.md` | the Wakey walkthrough's architecture notes and chapter contracts |
| `NOTES.md` | bugs and oddities found in the code while writing |
