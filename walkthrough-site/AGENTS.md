# Working on the walkthrough site

This folder is a Zola site that walks readers through source code. For humans, `README.md` explains how to run it and how to add content. This file is for anyone, human or AI, who writes or edits chapters.

## Read first

- **[LANGUAGE.md](LANGUAGE.md)** is the language guide. Every chapter follows it. Its main rule: give each idea room; do not compress.
- **[OUTLINE.md](OUTLINE.md)** has the architecture as understood, the chapter list and what each chapter covers.
- **`content/wakey/03-executor.md`** and **`04-startup.md`** are the model chapters. Match their shape and tone.

## Rules that do not bend

- **Accuracy.** Derive every statement from the code at the walkthrough's commit (`commit` in `content/<walkthrough>/_index.md`), not from what similar projects usually do. Read a file in full before writing about it. Read it with `git show <commit>:<path>` if the working tree may have moved on.
- **Excerpts are verbatim.** Never type or tidy code. Make every excerpt with `scripts/excerpt <walkthrough> <file> <first>-<last> [--hl "…"] [--skip "…"]` and paste its output unchanged. Leave lines out only with `--skip`, which writes the visible `// ...` marker. At most about 30 lines shown per excerpt (the check allows 32).
- **Do not change the project's source code.** Bugs and oddities go into `NOTES.md` in this folder, with file and line.
- **No layout in content.** Markdown and component calls only; no HTML in `content/`. Layout is in `templates/` and `sass/`.
- **Markdown: one line per paragraph.** No hard-wrapped prose.
- **Zola 0.23 with Tera 2.** There are no shortcodes; content calls components (`templates/components.html`). Do not use Tera 1 syntax from older examples.

## A chapter file

```toml
+++
title = "…"
description = "One sentence under the title."
weight = 4            # the chapter number; orders the chapters

[extra]
map = ["runtime"]     # parts of the walkthrough's map.svg to highlight (data-part names)
map_note = "One or two sentences on where this chapter is in the map."
goals = ["…"]         # "After this chapter you will understand": three to five
takeaways = ["…"]     # three to five full sentences
+++
```

The template renders the opener, the mini-map and the takeaways from this front matter, so the Markdown body holds only the tour. Inline code in these strings uses backticks.

## Components

```text
{% <concept title="…"> %} … {% </concept> %}
{% <tip title="…" id="…"> %} … {% </tip> %}
{% <pitfall title="…" id="…"> %} … {% </pitfall> %}
{% <unsafe title="…" id="…"> %} … {% </unsafe> %}
{{ <figure src="img/x.png" alt="…" caption="…" /> }}
{% <diagram caption="…"> %} mermaid source {% </diagram> %}
{{ <svg src="img/x.svg" caption="…" /> }}
{% <code file="…" lines="…" hl="…"> %}{% raw %} … {% endraw %}{% </code> %}   (from scripts/excerpt only)
```

Images and SVG files live in the walkthrough's `img/` folder. Hand-written SVGs use the classes in `sass/_blocks.scss` (`d-box`, `d-box--soft`, `d-box--accent`, `d-box--spark`, `d-box--dashed`, `d-line`, `d-line--spark`, `d-line--dashed`, `d-head`, `d-text`, `d-text--title`, `d-text--small`, `d-text--mono`, `d-text--on-accent`, `d-text--spark`), set no colours of their own, contain no blank lines, and give every `<marker>` an id that is unique across the site (prefix it with the file name). `img/executor.svg` is the example. Links to other chapters are written `[chapter 6](@/wakey/06-board.md)`.

## Check

```sh
scripts/check                 # build, links, excerpts, no external requests, reading times
scripts/check --time 230-290  # also: total reading time in range
```

When several writers work at the same time, each builds into a folder of its own (`zola build -o <dir> --force`), so that they do not overwrite each other's `public/`.

In content files, Tera reads `{{`, `{%` and `{#` as its own syntax. Excerpts are protected by `raw`; in prose and inline code, avoid these three sequences.

A chapter is done when `scripts/check` passes, it reads well against the checklist at the end of LANGUAGE.md, and it has been looked at in a browser in light and dark mode.
