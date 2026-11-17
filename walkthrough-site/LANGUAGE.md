# Language guide for walkthrough chapters

This guide says how the chapters are written. It exists because the first draft was too compressed: correct, but it made the reader do work the text should have done. Read it before writing or editing a chapter.

## Who reads this

Someone who knows Rust on paper (syntax, ownership, borrowing, lifetimes, traits, `async` in principle) and has built few real projects. They have never used an embedded board, Embassy, ESP-IDF, FreeRTOS or Slint. They read for two or three hours, a chapter at a time, and they are intelligent and willing. They cannot ask you what you meant.

## The main rule: give each idea room

Write as you would explain the code to a colleague sitting next to you, at a calm pace. If a sentence needs to be read twice, it is too dense. Size is not the constraint; understanding is.

**One idea per sentence.** When a sentence carries a fact, its reason and its consequence, split it.

> Too dense: Calling `app(spawner)` does not run `app`: the macro turns the `async fn` into a function that claims the task's static storage and returns a token, which fails only if every instance is running, hence the `unwrap`.
>
> Better: Calling `app(spawner)` does not run any of `app`'s code yet. The `#[embassy_executor::task]` macro has turned our `async fn` into an ordinary function, and that function only reserves the memory the task needs and hands back a token. The reservation can fail in one case: when the task is already running. That cannot happen here, because this is the only place that starts it, so the code simply calls `unwrap`.

**Say the step in between.** Do not jump from a line of code to a conclusion. If the reasoning is "A, therefore B, therefore C", write all three.

> Too dense: `clock::set_time_zone` changes the process environment, which is only safe while no other thread exists.
>
> Better: `clock::set_time_zone` sets an environment variable, `TZ`. Environment variables belong to the whole program, not to one thread, and the C library gives no protection when one thread changes them while another reads them. The code avoids the problem by its position: at this line no other thread has been started. The first one is spawned on the very next line.

**Keep the small words.** Write "because", "so", "which means", "that is why", "in other words". They are how the reader follows the argument. Do not replace them with a colon, a semicolon, a dash or an arrow.

**Explain a term before you use it.** The first time a chapter uses an embedded or library term, say what it is in a clause or a sentence: "NVS, the small key-value store ESP-IDF keeps in flash", "the C6, a second chip on the board that does the radio work", "PSRAM, the 32 MB of slower external memory". Do this once per chapter even if an earlier chapter did it; readers forget, and some skip. Never explain basic Rust syntax.

**Prefer the concrete.** "A page change takes 40 to 60 ms, during which nothing else on this thread runs" is better than "long frames block the executor". Use the names, numbers and file names from the code.

**Repeat what matters.** It is fine to say an important point in the text, show it in the code, and state it again in the takeaways. A guide repeats.

## How to walk through an excerpt

1. **Before the code, tell the reader what to look for.** One or two sentences: what this function is and which lines matter. "Here is the whole of `runtime::start`. The part to watch is the thread it spawns in the highlighted lines."
2. **Show the excerpt.** Highlight the lines the text talks about.
3. **After the code, go through it in the order the reader's eye moves**, top to bottom, naming the lines you mean ("line 151", "the `spawn` call"). Say what each part does, then why it is written this way, and where useful what the obvious alternative would have been and why the code does not do that.
4. **Close the loop.** One sentence on what this piece means for the chapter's question, before moving on.

Do not put two excerpts back to back without text between them.

## Voice

- Use "we" for the tour ("we now follow the handle into the thread") and "you" for the reader ("you will see this pattern again"). The code is "the code" or "the firmware"; do not invent an author's intentions beyond what a comment or the code itself shows.
- Present tense, active voice. "The thread polls the touch controller", not "the touch controller is polled".
- Plain words. "Use", not "leverage". "Shows", not "demonstrates". No "simply", "just", "obviously", "of course": what is obvious to the writer is not obvious to the reader.
- Paragraphs of two to five sentences. Start a new paragraph when the subject changes.
- British spelling, as in the repository's own documentation ("colour", "serialised").
- No rhetorical flourishes, no jokes that need context, no exclamation marks.

## Structure inside a chapter

- **Open each section by saying what it is about and why it comes now.** Close it by connecting to the next. The reader should never wonder "why am I reading this?".
- **Headings are plain statements of the topic**, not teasers.
- **Lists are for parallel items**, and each item is a full sentence or two. An argument is prose, not bullets.
- **Tables are for facts you look up** (the thread inventory), never for reasoning.
- **Link forward and back by chapter**, with a reason: "chapter 6 looks at this channel in detail".
- **Connect to the two themes.** Somewhere in every chapter, say plainly what it adds to "how do the parts work together" and "where does the state live".

## Callouts

A callout holds something that stands on its own and is worth finding again. It is two or three short paragraphs at most, written as loosely as the main text.

- `concept`: an idea from outside Rust the reader needs (an executor, an interrupt, I2C).
- `tip`: a Rust idiom, pattern or type-system technique the code in front of us uses.
- `pitfall`: something that goes wrong easily, shown by what the code does to avoid it.
- `unsafe`: an `unsafe` block and exactly what makes it sound.

Give every `tip`, `pitfall` and `unsafe` callout an `id`; the recap chapter links to it. Three to five callouts per chapter. A callout must not carry the main line of the explanation: the chapter has to make sense with all callouts skipped.

## Honesty

- Every statement comes from the code at the walkthrough's commit, a comment in it, or the repository's own documentation. If you did not read it, do not write it.
- Numbers such as "40 to 60 ms" come from comments and docs; say so when it matters ("the module's comment gives the figure").
- Where the code has a rough edge, say so in a sentence, explain the trade-off, and add it to `NOTES.md`. Do not criticise style.
- If you are not sure why something is the way it is, say what the code does and leave the why out. Do not guess.

## Length

A chapter is 10 to 15 minutes at 155 words a minute, counting everything in the file: at most about 2,300 words, with 1,800 to 2,100 as the target. Written loosely, that is room for 50 to 80 lines of code in excerpts of at most 30 lines, and three or four main ideas. If a chapter runs over, **cut scope, not explanation**: drop an excerpt, or move a side topic to the chapter that owns it (OUTLINE.md says which).

## Before you call a chapter done

- Read it from the top as the reader would. Mark every sentence you had to read twice and rewrite it.
- Check that every embedded or library term is explained where it first appears.
- Check that every excerpt has a sentence before it and a walk-through after it.
- Check that no step of an argument is left for the reader to work out.
- Run `scripts/check`.
