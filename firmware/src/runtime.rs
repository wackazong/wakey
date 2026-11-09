//! Embassy on ESP-IDF.
//!
//! The executor is `embassy-executor`'s std platform: it runs on a FreeRTOS
//! task and sleeps on a condition variable when no task is ready. Time
//! comes from the `embassy-time` driver of `esp-idf-svc` (esp_timer,
//! 1 MHz). Interrupts wake tasks through
//! [`esp_idf_svc::hal::interrupt::asynch::HalIsrNotification`], which moves
//! the waker call out of ISR context.

use embassy_executor::{Executor, Spawner};
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use static_cell::StaticCell;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Stack of the executor task (Slint rendering runs on it).
const EXECUTOR_STACK: usize = 64 * 1024;

/// Common start-up for every application binary: ESP-IDF runtime patches,
/// logger, then the Embassy executor on its own task, `app`.
///
/// Not on the main task: esp_hosted (Wi-Fi via the C6) allocates internal
/// RAM in a C constructor, before ESP-IDF creates the main task, and a
/// 64 KiB main stack then no longer fits beside the start-up stack (boot
/// loop in `esp_startup_start_app`). Once `main` runs, the start-up stack is
/// free again. `app` gets 64 KiB of internal RAM, priority 1 (as the main
/// task had) and core 0; the display ISRs are installed from it, so they
/// run on the same core as the render loop (see [`crate::display`]).
pub fn start(init: impl FnOnce(Spawner) + Send + 'static) -> ! {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    ThreadSpawnConfiguration {
        name: Some(c"app"),
        stack_size: EXECUTOR_STACK,
        priority: 1,
        pin_to_core: Some(Core::Core0),
        ..Default::default()
    }
    .set()
    .expect("thread config");
    let app = std::thread::Builder::new()
        .stack_size(EXECUTOR_STACK)
        .spawn(move || {
            let executor = EXECUTOR.init(Executor::new());
            executor.run(init)
        })
        .expect("app task");
    // Later threads get the ESP-IDF defaults again.
    ThreadSpawnConfiguration::default()
        .set()
        .expect("thread config");
    let _ = app.join();
    unreachable!("the executor never returns");
}
