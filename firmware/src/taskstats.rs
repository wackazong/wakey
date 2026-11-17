//! CPU time per task (feature `task-stats`): every 5 s logs the tasks that
//! used the most CPU since the last report, in percent of one core, and
//! the idle tasks' share (IDLE0, IDLE1: one per core). Needs FreeRTOS'
//! run-time statistics (`sdkconfig.defaults`).

use crate::runtime::Thread;
use esp_idf_svc::sys;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const PERIOD: Duration = Duration::from_secs(5);

/// Names of the running Rust threads by task handle (FreeRTOS calls them
/// all "pthread").
static NAMES: Mutex<BTreeMap<usize, &'static str>> = Mutex::new(BTreeMap::new());

fn names() -> MutexGuard<'static, BTreeMap<usize, &'static str>> {
    NAMES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wraps a thread's body so the statistics show it under `name` while it
/// runs; without the feature it only runs the body.
pub fn named<T>(name: &'static str, f: impl FnOnce() -> T) -> impl FnOnce() -> T {
    move || {
        if !cfg!(feature = "task-stats") {
            return f();
        }
        // SAFETY: plain FreeRTOS query.
        let task = unsafe { sys::xTaskGetCurrentTaskHandle() } as usize;
        names().insert(task, name);
        let result = f();
        // The handle can go to another task once this one has ended.
        names().remove(&task);
        result
    }
}

/// Starts the `task-stats` thread.
pub fn start() {
    Thread::new(c"task-stats", 6144)
        .spawn(|| {
            let mut last: BTreeMap<u32, u32> = BTreeMap::new();
            let mut last_total = 0u32;
            loop {
                std::thread::sleep(PERIOD);
                // SAFETY: plain FreeRTOS query.
                let n = unsafe { sys::uxTaskGetNumberOfTasks() } as usize + 4;
                let mut tasks: Vec<sys::TaskStatus_t> = Vec::with_capacity(n);
                let mut total = 0u32;
                // SAFETY: `uxTaskGetSystemState` fills at most `n` entries
                // of `tasks` (room for tasks created meanwhile) and returns
                // how many: those are initialised.
                unsafe {
                    let got = sys::uxTaskGetSystemState(tasks.as_mut_ptr(), n as _, &mut total);
                    tasks.set_len(got as usize);
                }
                // Counters wrap (32 bit, µs): differences stay right.
                let span = u64::from(total.wrapping_sub(last_total).max(1));
                let mut now = BTreeMap::new();
                let mut used: Vec<(u64, String)> = Vec::new();
                let names = names().clone();
                for t in &tasks {
                    let name = match names.get(&(t.xHandle as usize)) {
                        Some(n) => (*n).to_string(),
                        // SAFETY: FreeRTOS task names are NUL-terminated
                        // and live with their task.
                        None => unsafe { CStr::from_ptr(t.pcTaskName) }
                            .to_string_lossy()
                            .into_owned(),
                    };
                    let key = t.xTaskNumber;
                    let run = t.ulRunTimeCounter;
                    let delta = u64::from(run.wrapping_sub(*last.get(&key).unwrap_or(&run)));
                    now.insert(key, run);
                    used.push((delta, name));
                }
                last = now;
                last_total = total;
                used.sort_by_key(|(delta, _)| core::cmp::Reverse(*delta));
                let line: Vec<String> = used
                    .iter()
                    .take(8)
                    .map(|(d, name)| format!("{name} {}%", d * 100 / span))
                    .collect();
                log::info!("tasks: {}", line.join(", "));
            }
        })
        .expect("task-stats thread");
}
