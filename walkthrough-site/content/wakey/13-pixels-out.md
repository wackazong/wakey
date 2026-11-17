+++
title = "The render loop: pixels out"
description = "From Slint's lines to the glass: two frame buffers, a rotation in hardware, and an interrupt that sets the pace."
weight = 13

[extra]
map = ["ui", "display"]
map_note = "The render loop is in `ui`. Everything it draws passes through `display`, the hardware layer's module for the panel, its two frame buffers and the PPA."
goals = [
  "which path a pixel takes from Slint to the panel",
  "why there are two frame buffers, and what a flip is",
  "how an interrupt from the panel sets the pace of the loop",
  "why Slint must be told about the two buffers, and what `other_buffer_stale` is for",
]
takeaways = [
  "The panel is fed from one of two frame buffers in PSRAM. The loop draws into the other one and then swaps them with `Display::present`.",
  "`Display::back_buffer().await` returns only after the frame-complete interrupt has counted a full frame since the flip.",
  "Slint fills 16-line strips through `PpaLines`, and the PPA rotates each strip into the back buffer.",
  "With two buffers, every change must be drawn twice. `RepaintBufferType::SwappedBuffers` tells Slint so, and `other_buffer_stale` makes the second render happen at once.",
]
+++

The [previous chapter](@/wakey/12-touch-in.md) read the first half of `render_loop::run`: touch events come in, Slint's timers run, and the loop sleeps when nothing changes. This chapter reads the second half of the same function. It starts where the loop asks for a buffer to draw into, and it ends with the picture on the glass.

## The path of a pixel

A *frame buffer* is a block of memory that holds one complete picture, pixel by pixel. Here a pixel is a `u16` in the RGB565 format, and the panel has 720 × 1280 of them, so one frame buffer is about 1.8 MB. That is too large for the chip's internal RAM, so both frame buffers live in *PSRAM*, the board's 32 MB of slower external memory.

*DMA* is hardware that copies data on its own, without the processor. The display driver uses it to send a frame buffer to the panel again and again, about 66 times a second on the ST7123 panel. The CPU never copies pixels to the panel.

The *PPA*, the Pixel Processing Accelerator, is a block of the ESP32-P4 that can scale, rotate and mirror a picture. The firmware needs it because the panel is built in portrait, 720 pixels wide and 1280 high, while the user interface is in landscape, 1280 wide and 720 high. Every picture has to be turned by 90° on its way out.

{% <diagram caption="The display pipeline. Slint and `dim::apply` run on the CPU. The rotation and the stream to the panel are done by hardware."> %}
flowchart TD
    S[Slint renders one landscape line] --> ST[strip of 16 lines, internal RAM]
    ST --> D[dim::apply dims the line in the strip]
    D -->|PPA rotates by 90 degrees| BB[back buffer, PSRAM]
    BB -->|flip in present| FB[front buffer, PSRAM]
    FB -->|DMA stream at 66 Hz| P[panel]
{% </diagram> %}

## Two frame buffers and the flip

If the loop drew into the buffer that the DMA is sending, the panel would show half-finished pictures. So there are two buffers. The *front* buffer is the one on screen. The *back* buffer is the one the loop may draw into. When a frame is finished they change roles, and that is called a *flip*.

Here is the place in the loop where the output half begins. Line 307 is the one to watch.

{% <code file="firmware/src/ui/render_loop.rs" lines="305-308" hl="307"> %}{% raw %}
        // Waits for the previous flip, so this paces to the panel refresh.
        enter(Stage::BackBuffer);
        let fb = display.back_buffer().await.as_mut_ptr();
        let target = Target::frame_buffer(Picture::frame_buffer(fb));
{% endraw %}{% </code> %}

Line 307 asks the display for the back buffer and waits for it. The loop takes only a raw pointer from the slice, and line 308 wraps it in a `Target`, the description of a picture the PPA can write into.

When the frame is drawn, the loop calls `display.present`. That function hands the driver a pointer to the back buffer. The driver recognises its own buffer and only switches to it; it copies nothing. Then `present` notes what it has done in two fields of `Display`: `front` now names the other buffer, and `pending_flip` holds the number of frames the panel has completed so far.

The switch does not take effect at once. The DMA is in the middle of sending the old front buffer, and it picks up the new one at the start of the next frame. Until then nobody may draw into the old front buffer, and `pending_flip` is the note that says so.

## The interrupt that sets the pace

How does the loop learn that the flip has happened? An *interrupt* arrives each time the DMA has finished a frame. An interrupt is a signal from hardware that makes the processor stop what it is doing and run a short function, the interrupt handler or ISR. This is the display's handler.

{% <code file="firmware/src/display/mod.rs" lines="199-208" hl="204 206"> %}{% raw %}
unsafe extern "C" fn on_frame_buf_complete(
    _panel: sys::esp_lcd_panel_handle_t,
    _edata: *mut sys::esp_lcd_dpi_panel_event_data_t,
    _user_ctx: *mut c_void,
) -> bool {
    FRAMES_DONE.fetch_add(1, Ordering::Release);
    interrupt::with_isr_yield_signal(|| {
        FRAME_DONE.notify_lsb();
    })
}
{% endraw %}{% </code> %}

The C driver calls this function, which is why it is `unsafe extern "C"`. It does two things. Line 204 adds one to `FRAMES_DONE`, an atomic counter of completed frames. Line 206 notifies `FRAME_DONE`, a static of type `HalIsrNotification` from the `esp-idf-hal` crate. Its `notify_lsb` may be called from an interrupt, and it wakes the task that waits in `FRAME_DONE.wait()`.

The handler does nothing else. That is the pattern [chapter 9](@/wakey/09-flags-and-locks.md) named: an interrupt ends in an atomic or a notification. The other side is `back_buffer`, which [chapter 4](@/wakey/04-startup.md) showed as the task's first `await`.

{% <code file="firmware/src/display/mod.rs" lines="543-555" hl="546-547 554"> %}{% raw %}
    pub async fn back_buffer(&mut self) -> &mut [u16] {
        if let Some(requested_at) = self.pending_flip {
            // The flip is done once one full frame ended after the request.
            while Self::frames_scanned().wrapping_sub(requested_at) as i32 <= 0 {
                FRAME_DONE.wait().await;
            }
            self.pending_flip = None;
        }
        let back = 1 - self.front;
        // SAFETY: the driver allocated PIXELS u16 per buffer; the DMA only
        // reads the front buffer, and `&mut self` prevents aliasing.
        unsafe { core::slice::from_raw_parts_mut(self.fbs[back], PIXELS) }
    }
{% endraw %}{% </code> %}

If no flip is pending, the function skips the `if` and returns the back buffer at once. Otherwise line 546 compares the counter with the value `present` stored. The flip is done when the counter has moved past that value, which means one full frame has ended since the request. Until then, line 547 waits. The task gives control back to the executor, and the `app` thread sleeps until the interrupt handler notifies it. The check is a `while`, so the counter is read again after every wake-up.

This one `await` paces the whole user interface. A frame of the panel takes about 15 ms. However quickly the loop draws, its next `back_buffer().await` does not return before the panel has taken over the last picture.

{% <pitfall title="The handler and the loop share a core" id="display-isr-same-core"> %}
`present` switches buffers and then reads the frame counter, and the handler changes that counter. The doc comment of `present` therefore has a rule: call it from the core the interrupt runs on. On one core the handler runs as a whole, before or after each of the two steps. It is never halfway through on a second core while `present` reads the counter.

The driver installs the handler on the core that creates the panel. That happens on the `app` thread, which is pinned to core 0, and the render loop runs on the same thread.
{% </pitfall> %}

{% <unsafe title="A slice over the driver's frame buffer" id="unsafe-display-back-buffer"> %}
Line 554 builds a `&mut [u16]` from a raw pointer. The comment names what makes it sound. The driver allocated `PIXELS` values for each buffer, so the length is right. And nobody else writes there: the DMA only reads the *front* buffer, and `&mut self` means the caller cannot hold a second slice.
{% </unsafe> %}

## Slint draws lines, the PPA turns them

Now the loop has a buffer, and Slint can draw. Its software renderer can draw line by line into memory we lend it. These lines come from the body of a closure that the loop passes to `window.draw_if_needed`, and it runs only if a redraw is due. Look at the three calls at the end.

{% <code file="firmware/src/ui/render_loop.rs" lines="330-339" hl="333 337-339"> %}{% raw %}
            renderer.set_repaint_buffer_type(if full_repaints > 0 {
                RepaintBufferType::NewBuffer
            } else {
                RepaintBufferType::SwappedBuffers
            });
            full_repaints = full_repaints.saturating_sub(1);
            renderer.set_rendering_rotation(RenderingRotation::NoRotation);
            rot.begin(target);
            let region = renderer.render_by_line(PpaLines { rot: &mut *rot });
            rot.finish();
{% endraw %}{% </code> %}

Lines 330 to 334 choose a repaint mode, which is the subject of the next section. Line 336 tells Slint not to rotate; the PPA does that. Then come three steps: `rot.begin(target)` tells the rotator where the picture goes, `render_by_line` lets Slint draw, and `rot.finish()` waits until the PPA has written everything.

`render_by_line` takes a value that implements Slint's trait `LineBufferProvider`, here `PpaLines`. Slint calls its one method for every line it wants to draw.

{% <code file="firmware/src/ui/platform.rs" lines="97-111" hl="107 109 110"> %}{% raw %}
impl LineBufferProvider for PpaLines<'_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        let x0 = range.start;
        let px = self.rot.line(line, range);
        // SAFETY: Rgb565Pixel is #[repr(transparent)] over u16.
        render_fn(unsafe { core::slice::from_raw_parts_mut(px.as_mut_ptr().cast(), px.len()) });
        dim::apply(px, x0, line);
    }
{% endraw %}{% </code> %}

Slint passes the line number, the range of pixels in that line that need drawing, and a function `render_fn` that fills them. Our job is to supply the memory. Line 107 gets it from the rotator: `rot.line` returns a slice inside a *strip*, a buffer of 16 landscape lines in internal RAM. Line 109 lets Slint draw into that slice. The strip is a `[u16]` and Slint wants a `[Rgb565Pixel]`, so the line casts the pointer. The comment says why that is allowed: both types have the same layout.

Line 110 is the night dimming. In a dark room the backlight's lowest level is still too bright, so `dim::apply` scales every pixel of the fresh line down.

What happens to a full strip is in `display/ppa.rs`. The rotator hands the strip to the PPA with a non-blocking call, or with two for a strip wider than 640 pixels. The PPA turns it by 90° and writes it into the back buffer by DMA, while the CPU already renders the next strip into another buffer. The module's comment gives the reason: it saves the CPU the rotation in software and the copy into PSRAM. The PPA also reports completion by an interrupt, and that handler too only adds one to an atomic counter.

## Every change is drawn twice

Slint repaints only what changed. With two buffers that is not enough. Say the minute changes and Slint draws the new digits into buffer A. Buffer B still holds the old minute, and after the next flip the panel would show it again.

`RepaintBufferType::SwappedBuffers` on line 333 tells Slint about this. In that mode Slint repaints what changed in the last two frames, so each buffer receives every change once. The other mode, `NewBuffer`, repaints everything; the loop uses it twice after the display wakes from sleep, because both buffers are blank then.

One problem remains. After a change, the second buffer is only brought up to date by the *next* render. If a touch causes that render, the touch pays for the old work as well. The comment at the variable gives a figure of about 60 ms for a page switch. So the loop does not wait.

{% <code file="firmware/src/ui/render_loop.rs" lines="379-387" hl="379 387"> %}{% raw %}
        other_buffer_stale = slint_box.is_some() && !catching_up;
        if slint_rendered || hook_started {
            // ... @387
            display.present(0..1).expect("present");
{% endraw %}{% </code> %}

Line 379 sets `other_buffer_stale` when Slint drew something and this frame was not itself the catch-up. Then the loop does not go to sleep. On its next turn it copies the flag into `catching_up` and calls `window.request_redraw()`, so Slint repaints the same region into the other buffer while nothing else is happening.

Line 387 is the flip. Its argument names the rows that the CPU wrote and that must be flushed from its cache to PSRAM first. The CPU wrote none, the PPA did, so the loop passes a single row, the least the driver accepts.

## Two extensions

The loop's fourth argument is an optional `FrameHook`, which the previous chapter introduced. The alarm list uses it. While the list scrolls, `ui/list_layer` copies ready-made pictures of its rows into the back buffer with a second DMA engine, the 2D-DMA. The hook's `start` and `finish` stand around Slint's own render.

Page changes work in a similar way. `ui/transition.rs` renders the new page once into a picture of its own and slides the old and the new picture across the frame buffers, again by DMA. Afterwards the loop sets `other_buffer_stale`, for the reason we have just seen.

For our two questions: the parts work together here through one counter and one notification, written by an interrupt and awaited by the task. The state is small and has one owner. `Display` knows which buffer is in front and whether a flip is pending, and the render loop took `Display` by value. The [next chapter](@/wakey/14-state-machine.md) turns from pixels to the state they show.
