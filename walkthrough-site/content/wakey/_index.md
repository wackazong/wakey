+++
title = "Wakey"
description = "A bedside alarm clock in Rust on an ESP32-P4: how its threads, its one async task and its state fit together, read from the real source."
sort_by = "weight"
weight = 1

[extra]
kicker = "Rust · embedded · walkthrough"
tags = ["Rust", "ESP-IDF", "Embassy", "Slint", "ESP32-P4"]
# The commit every excerpt is taken from, and where the file names link to.
commit = "888a2a19da0f9dc0081ce345797b496f491d0d94"
repo = "https://github.com/wackazong/wakey"
# The "where we are" map, in this folder.
map = "map.svg"
# Beside the title on the overview, the first in front (16:9 screenshots).
shots = [
  { src = "img/ui-clock-sunrise.jpg", alt = "The clock face at sunrise: 05:35 in large digits on an orange sky, the next alarm below" },
  { src = "img/ui-alarm-page.png", alt = "The alarm page with hour and minute wheels, weekdays and sound settings" },
  { src = "img/ui-sound-tidal.png", alt = "The sound page with tracks from a TIDAL playlist" },
]
# The night sky and the sunrise of the clock's own face.
accent = "#3a1d6e"
spark = "#ff8f3f"
wpm = 155
+++

Wakey is the firmware of a bedside alarm clock: a 5-inch touch screen with a sky that follows the sun, alarms set by touch, and wake-up sounds from a memory card, a TIDAL playlist or AirDrop. It is about 25,000 lines of Rust on an M5Stack Tab5.

This walkthrough reads that code with you, from the top down: what the device is for, how the firmware is layered, what runs at the same time, and then the individual threads and functions. Two questions run through every chapter: **how do the parts work together**, and **where does the application's state live?**

It is a tour, not a tutorial. You build nothing. Every code block is a verbatim excerpt from the repository at one commit, with its file and line numbers, and the text says why the code is shaped the way it is, including where it has rough edges.

## What you should know

- Rust on paper: ownership and borrowing, lifetimes, traits and generics, closures, `Arc`, `Mutex`, `Rc`, `RefCell`.
- `async` and `await` in principle: that a future does nothing until it is polled.
- How to read a `match`.

## What you do not need

- Embedded experience. Chips, buses and interrupts are explained where the code meets them.
- Embassy, ESP-IDF or FreeRTOS.
- Slint. One short introduction in chapter 10 is enough to follow the Rust that talks to it.

## How to read it

The chapters build on each other, so read them in order the first time. Each takes 10 to 15 minutes and ends with its key takeaways. Rust idioms and pitfalls are marked where they occur and collected in the last chapter.
