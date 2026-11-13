//! CPU time per task (feature `task-stats`): every 5 s logs the tasks that
//! used the most CPU since the last report, in percent of one core, and
//! the idle tasks' share (IDLE0, IDLE1: one per core). Needs FreeRTOS' run-time statistics
//! (`sdkconfig.defaults`).

use esp_idf_svc::sys;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::sync::Mutex;
use std::time::Duration;

/// Names of Rust threads by task handle (FreeRTOS calls them all
/// "pthread").
static NAMES: Mutex<BTreeMap<usize, &'static str>> = Mutex::new(BTreeMap::new());

/// Wraps a thread's body so the statistics show it under `name`.
pub fn named<T>(name: &'static str, f: impl FnOnce() -> T) -> impl FnOnce() -> T {
    move || {
        let task = unsafe { sys::xTaskGetCurrentTaskHandle() } as usize;
        NAMES.lock().unwrap_or_else(|e| e.into_inner()).insert(task, name);
        f()
    }
}

const PERIOD: Duration = Duration::from_secs(5);

/// Starts the `task-stats` thread.
pub fn start() {
    std::thread::Builder::new()
        .name("task-stats".into())
        .stack_size(6144)
        .spawn(|| {
            let mut last: BTreeMap<u32, u32> = BTreeMap::new();
            let mut last_total = 0u32;
            loop {
                std::thread::sleep(PERIOD);
                let n = unsafe { sys::uxTaskGetNumberOfTasks() } as usize + 4;
                let mut tasks: Vec<sys::TaskStatus_t> = Vec::with_capacity(n);
                let mut total = 0u32;
                let got = unsafe {
                    sys::uxTaskGetSystemState(tasks.as_mut_ptr(), n as _, &mut total)
                } as usize;
                unsafe { tasks.set_len(got) };
                // Counters wrap (32 bit, µs): differences stay right.
                let span = u64::from(total.wrapping_sub(last_total).max(1));
                let mut now = BTreeMap::new();
                let mut used: Vec<(u64, String)> = Vec::new();
                let names = NAMES.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for t in &tasks {
                    let name = match names.get(&(t.xHandle as usize)) {
                        Some(n) => (*n).to_string(),
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
