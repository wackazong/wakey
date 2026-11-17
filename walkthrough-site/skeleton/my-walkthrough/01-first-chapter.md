+++
title = "The first chapter"
description = "One sentence that goes under the title and into the chapter list."
# The chapter's number: chapters are ordered by it.
weight = 1

[extra]
# Parts of map.svg to highlight (their `data-part` names).
map = ["one"]
map_note = "One or two sentences on where this chapter is in the map."
# "After this chapter you will understand": three to five.
goals = [
  "the first thing, with `code` in backticks where needed",
  "the second thing",
  "the third thing",
]
# Three to five full sentences.
takeaways = [
  "The first point the reader should keep.",
  "The second point.",
  "The third point.",
]
+++

An opening paragraph: what this chapter is about and why it comes now.

## A section

Tell the reader what to look for, then show the code. Make the excerpt with `scripts/excerpt my-walkthrough src/main.rs 1-12 --hl "3-5"` and paste what it prints here, unchanged.

After the excerpt, go through it top to bottom.

{% <concept title="An idea from outside the language"> %}
Two or three short paragraphs.
{% </concept> %}

{% <tip title="An idiom the code uses" id="a-unique-id"> %}
What it is, and why the code uses it here.
{% </tip> %}

{% <pitfall title="Something that goes wrong easily" id="another-id"> %}
What goes wrong, and what the code does about it.
{% </pitfall> %}

{% <unsafe title="An unsafe block" id="unsafe-something"> %}
What the block does, and exactly what makes it sound.
{% </unsafe> %}

{{ <figure src="img/picture.png" alt="What the picture shows" caption="A caption, with `code` if needed." /> }}

{% <diagram caption="A Mermaid diagram."> %}
sequenceDiagram
    participant A as one
    participant B as two
    A->>B: a message
    B-->>A: an answer
{% </diagram> %}

{{ <svg src="map.svg" caption="A hand-written SVG from this folder." /> }}

A closing paragraph that connects to the next chapter.
