//! Threads and the Embassy executor on ESP-IDF.
//!
//! [`Thread`] spawns a `std` thread with the FreeRTOS settings `std` cannot
//! express (core, priority, stack in PSRAM). The executor is `embassy-executor`'s std platform: it runs on a FreeRTOS
//! task and sleeps on a condition variable when no task is ready. Time
//! comes from the `embassy-time` driver of `esp-idf-svc` (esp_timer,
//! 1 MHz). Interrupts wake tasks through
//! [`esp_idf_svc::hal::interrupt::asynch::HalIsrNotification`], which moves
//! the waker call out of ISR context.

use embassy_executor::{Executor, Spawner};
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::task::thread::{MallocCap, ThreadSpawnConfiguration};
use static_cell::StaticCell;
use std::ffi::CStr;
use std::thread::{JoinHandle, Scope, ScopedJoinHandle};

/// A thread to spawn: its name (for FreeRTOS and [`crate::taskstats`]),
/// stack size and, unless ESP-IDF's defaults do, core, priority and where
/// its stack lives.
#[derive(Clone, Copy)]
pub struct Thread {
    name: &'static CStr,
    stack_size: usize,
    core: Option<Core>,
    priority: Option<u8>,
    stack_in_psram: bool,
}

impl Thread {
    pub const fn new(name: &'static CStr, stack_size: usize) -> Self {
        Self {
            name,
            stack_size,
            core: None,
            priority: None,
            stack_in_psram: false,
        }
    }

    /// Pinned to `core`.
    pub const fn core(self, core: Core) -> Self {
        Self {
            core: Some(core),
            ..self
        }
    }

    /// FreeRTOS priority (the UI task runs at 1).
    pub const fn priority(self, priority: u8) -> Self {
        Self {
            priority: Some(priority),
            ..self
        }
    }

    /// The stack in PSRAM: for large stacks of threads that are not
    /// timing-critical.
    pub const fn stack_in_psram(self) -> Self {
        Self {
            stack_in_psram: true,
            ..self
        }
    }

    pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.configured(|name| {
            std::thread::Builder::new()
                .name(name.into())
                .stack_size(self.stack_size)
                .spawn(crate::taskstats::named(name, f))
        })
    }

    pub fn spawn_scoped<'scope, 'env, F, T>(
        self,
        scope: &'scope Scope<'scope, 'env>,
        f: F,
    ) -> std::io::Result<ScopedJoinHandle<'scope, T>>
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        self.configured(|name| {
            std::thread::Builder::new()
                .name(name.into())
                .stack_size(self.stack_size)
                .spawn_scoped(scope, crate::taskstats::named(name, f))
        })
    }

    /// Runs `spawn` with this thread's configuration set for the calling
    /// thread's spawns (ESP-IDF keeps it per thread), then restores the
    /// previous one.
    fn configured<R>(self, spawn: impl FnOnce(&'static str) -> R) -> R {
        let name = self.name.to_str().unwrap_or("thread");
        let previous = ThreadSpawnConfiguration::get().unwrap_or_default();
        let defaults = ThreadSpawnConfiguration::default();
        let config = ThreadSpawnConfiguration {
            name: Some(self.name),
            stack_size: self.stack_size,
            priority: self.priority.unwrap_or(defaults.priority),
            pin_to_core: self.core,
            stack_alloc_caps: if self.stack_in_psram {
                MallocCap::Spiram | MallocCap::Cap8bit
            } else {
                defaults.stack_alloc_caps
            },
            ..defaults
        };
        if let Err(e) = config.set() {
            log::warn!("{name}: thread configuration not set: {e}");
        }
        let result = spawn(name);
        if let Err(e) = previous.set() {
            log::warn!("{name}: thread configuration not restored: {e}");
        }
        result
    }
}

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

    let app = Thread::new(c"app", EXECUTOR_STACK)
        .core(Core::Core0)
        .priority(1)
        .spawn(move || {
            let executor = EXECUTOR.init(Executor::new());
            executor.run(init)
        })
        .expect("app task");
    let _ = app.join();
    unreachable!("the executor never returns");
}
