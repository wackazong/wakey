+++
# The walkthrough's name: on its card, its overview and in the sidebar.
title = "My walkthrough"
# One or two sentences: on the card and under the title of the overview.
description = "What the project is, and what the reader will have seen at the end."
# Chapters are ordered by their `weight`.
sort_by = "weight"
# Orders the cards on the landing page.
weight = 2

[extra]
# The small line above the title.
kicker = "Rust · walkthrough"
# Shown on the card.
tags = ["Rust"]
# The commit every excerpt is taken from. scripts/excerpt and
# scripts/verify-excerpts read the code at this commit.
commit = "0000000000000000000000000000000000000000"
# The Git repository the commit is in, as a path from the site folder.
# Leave it out if the site lives inside that repository.
# source = "../../other-project"
# Where a file name above an excerpt links to. Leave it out for no links.
# repo = "https://github.com/someone/project"
# The "where we are" map in this folder. Leave it out for no mini-map.
map = "map.svg"
# Two colours: `accent` for the blocks (white text must be readable on
# it), `spark` for highlights (dark text must be readable on it).
accent = "#1f3a5f"
spark = "#f2a93b"
# Reading speed in words per minute, code included (default: zola.toml).
# wpm = 155
+++

A summary of the project in two or three paragraphs: what it is, how large it is, and what this walkthrough does with it.

## What you should know

- What the reader must bring.

## What you do not need

- What the walkthrough explains on the way.
