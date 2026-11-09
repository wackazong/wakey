//! Poor man's sampling profiler for core 0.
//!
//! A GPTimer interrupt at [`RATE_HZ`] records the interrupted program
//! counter (`mepc`). [`report`] logs the most frequent addresses; resolve
//! them to functions on the host with `scripts/profile.sh <elf> <log>`.
//!
//! Start it from the core you want to sample (the timer ISR is installed on
//! the calling core). Samples taken while the core is idle land in the
//! FreeRTOS idle task.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use esp_idf_svc::sys::{self as sys, esp};

pub const RATE_HZ: u32 = 5_000;
const MAX_SAMPLES: usize = 16_384;

/// Sample buffer, allocated in [`start`] (64 KiB, ends up in PSRAM).
static SAMPLES: AtomicPtr<u32> = AtomicPtr::new(ptr::null_mut());
static COUNT: AtomicUsize = AtomicUsize::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);

#[inline(always)]
fn mepc() -> u32 {
    let pc: u32;
    unsafe { core::arch::asm!("csrr {0}, mepc", out(reg) pc) };
    pc
}

unsafe extern "C" fn on_alarm(
    _timer: sys::gptimer_handle_t,
    _edata: *const sys::gptimer_alarm_event_data_t,
    _ctx: *mut c_void,
) -> bool {
    if RUNNING.load(Ordering::Relaxed) {
        let i = COUNT.load(Ordering::Relaxed);
        if i < MAX_SAMPLES {
            unsafe { *SAMPLES.load(Ordering::Relaxed).add(i) = mepc() };
            COUNT.store(i + 1, Ordering::Relaxed);
        }
    }
    false
}

/// Starts sampling the calling core.
pub fn start() {
    let buf: &'static mut [u32] = Vec::leak(vec![0u32; MAX_SAMPLES]);
    SAMPLES.store(buf.as_mut_ptr(), Ordering::Release);
    unsafe {
        let mut timer: sys::gptimer_handle_t = ptr::null_mut();
        let mut cfg: sys::gptimer_config_t = core::mem::zeroed();
        cfg.clk_src = sys::soc_periph_gptimer_clk_src_t_GPTIMER_CLK_SRC_DEFAULT;
        cfg.direction = sys::gptimer_count_direction_t_GPTIMER_COUNT_UP;
        cfg.resolution_hz = 1_000_000;
        esp!(sys::gptimer_new_timer(&cfg, &mut timer)).expect("profiler timer");
        let cbs = sys::gptimer_event_callbacks_t {
            on_alarm: Some(on_alarm),
        };
        esp!(sys::gptimer_register_event_callbacks(
            timer,
            &cbs,
            ptr::null_mut()
        ))
        .expect("profiler cb");
        let mut alarm: sys::gptimer_alarm_config_t = core::mem::zeroed();
        alarm.alarm_count = u64::from(1_000_000 / RATE_HZ);
        alarm.reload_count = 0;
        alarm.flags.set_auto_reload_on_alarm(1);
        esp!(sys::gptimer_set_alarm_action(timer, &alarm)).expect("profiler alarm");
        esp!(sys::gptimer_enable(timer)).expect("profiler enable");
        esp!(sys::gptimer_start(timer)).expect("profiler start");
    }
    COUNT.store(0, Ordering::Relaxed);
    RUNNING.store(true, Ordering::Release);
    log::info!("profiler: sampling core 0 at {RATE_HZ} Hz");
}

/// Stops sampling and logs the top addresses (`profile: <hits> 0x<pc>`).
pub fn report(top: usize) {
    RUNNING.store(false, Ordering::Release);
    let n = COUNT.load(Ordering::Acquire);
    // SAFETY: sampling has stopped, the ISR no longer writes.
    let samples = unsafe { core::slice::from_raw_parts_mut(SAMPLES.load(Ordering::Acquire), n) };
    samples.sort_unstable();
    let mut hist: Vec<(u32, u32)> = Vec::new();
    for &pc in samples.iter() {
        match hist.last_mut() {
            Some((p, c)) if *p == pc => *c += 1,
            _ => hist.push((pc, 1)),
        }
    }
    hist.sort_unstable_by_key(|&(_, hits)| core::cmp::Reverse(hits));
    log::info!("profile: {n} samples");
    for (pc, hits) in hist.iter().take(top) {
        log::info!("profile: {hits} 0x{pc:08x}");
    }
    log::info!("profile: end");
}
