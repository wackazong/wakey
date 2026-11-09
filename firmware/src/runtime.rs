//! Embassy on ESP-IDF.
//!
//! The executor is `embassy-executor`'s std platform: it runs on the calling
//! FreeRTOS task and sleeps on a condition variable when no task is ready.
//! Time comes from the `embassy-time` driver of `esp-idf-svc` (esp_timer,
//! 1 MHz). Interrupts wake tasks through
//! [`esp_idf_svc::hal::interrupt::asynch::HalIsrNotification`], which moves
//! the waker call out of ISR context.

use embassy_executor::{Executor, Spawner};
use static_cell::StaticCell;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Common start-up for every application binary: ESP-IDF runtime patches,
/// logger, then the Embassy executor on the current (main) task.
///
/// The main task has `CONFIG_ESP_MAIN_TASK_STACK_SIZE` (64 KiB) of stack and
/// runs on core 0. The display ISRs are installed from here too, so they run
/// on the same core as the render loop (see [`crate::display`]).
pub fn start(init: impl FnOnce(Spawner)) -> ! {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let executor = EXECUTOR.init(Executor::new());
    executor.run(init)
}
