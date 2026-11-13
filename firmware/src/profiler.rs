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
/// The interrupted code's return address per sample (its caller, if it is
/// a leaf at that moment): [`report`] lists them for samples in
/// `vPortExitCriticalMultiCore`, which stand for time spent with
/// interrupts off, to show who turned them off.
static CALLERS: AtomicPtr<u32> = AtomicPtr::new(ptr::null_mut());

unsafe extern "C" {
    /// FreeRTOS (tasks.c): the running task per core; the first field of a
    /// TCB is its saved stack pointer, written on interrupt entry
    /// (`rtos_int_enter`), where the frame holds `mepc`, then `ra`.
    static pxCurrentTCBs: [*const *const u32; 2];
    fn vPortExitCriticalMultiCore(mux: *mut c_void);
}

/// `ra` of the interrupted task, 0 if its saved stack pointer looks wrong.
#[inline(always)]
fn interrupted_ra() -> u32 {
    // SAFETY: read-only peeks at FreeRTOS state; the pointers are checked
    // for plausible RAM addresses before they are followed.
    unsafe {
        let core: usize;
        core::arch::asm!("csrr {0}, mhartid", out(reg) core);
        let tcb = ptr::read_volatile(ptr::addr_of!(pxCurrentTCBs).cast::<*const *const u32>().add(core & 1));
        if tcb.is_null() {
            return 0;
        }
        let sp = ptr::read_volatile(tcb);
        let a = sp as usize;
        if !(0x4800_0000..0x5000_0000).contains(&a) || a % 4 != 0 {
            return 0;
        }
        ptr::read_volatile(sp.add(1))
    }
}
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
            unsafe {
                *SAMPLES.load(Ordering::Relaxed).add(i) = mepc();
                *CALLERS.load(Ordering::Relaxed).add(i) = interrupted_ra();
            }
            COUNT.store(i + 1, Ordering::Relaxed);
        }
    }
    false
}

/// Starts sampling the calling core at [`RATE_HZ`].
pub fn start() {
    start_at(RATE_HZ);
}

/// Starts sampling the calling core at `rate_hz` ([`MAX_SAMPLES`] samples
/// at most: lower rates for longer runs).
pub fn start_at(rate_hz: u32) {
    let buf: &'static mut [u32] = Vec::leak(vec![0u32; MAX_SAMPLES]);
    SAMPLES.store(buf.as_mut_ptr(), Ordering::Release);
    let callers: &'static mut [u32] = Vec::leak(vec![0u32; MAX_SAMPLES]);
    CALLERS.store(callers.as_mut_ptr(), Ordering::Release);
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
        alarm.alarm_count = u64::from(1_000_000 / rate_hz);
        alarm.reload_count = 0;
        alarm.flags.set_auto_reload_on_alarm(1);
        esp!(sys::gptimer_set_alarm_action(timer, &alarm)).expect("profiler alarm");
        esp!(sys::gptimer_enable(timer)).expect("profiler enable");
        esp!(sys::gptimer_start(timer)).expect("profiler start");
    }
    COUNT.store(0, Ordering::Relaxed);
    RUNNING.store(true, Ordering::Release);
    log::info!("profiler: sampling at {rate_hz} Hz");
}

/// Stops sampling and logs the top addresses (`profile: <hits> 0x<pc>`).
pub fn report(top: usize) {
    RUNNING.store(false, Ordering::Release);
    let n = COUNT.load(Ordering::Acquire);
    // SAFETY: sampling has stopped, the ISR no longer writes.
    let samples = unsafe { core::slice::from_raw_parts_mut(SAMPLES.load(Ordering::Acquire), n) };
    let callers = unsafe { core::slice::from_raw_parts_mut(CALLERS.load(Ordering::Acquire), n) };
    let exit = vPortExitCriticalMultiCore as *const () as usize as u32;
    let mut critical: Vec<u32> = samples
        .iter()
        .zip(callers.iter())
        .filter(|&(&pc, _)| pc.wrapping_sub(exit) < 512)
        .map(|(_, &ra)| ra)
        .collect();
    critical.sort_unstable();
    let mut by_caller: Vec<(u32, u32)> = Vec::new();
    for &ra in &critical {
        match by_caller.last_mut() {
            Some((p, c)) if *p == ra => *c += 1,
            _ => by_caller.push((ra, 1)),
        }
    }
    by_caller.sort_unstable_by_key(|&(_, hits)| core::cmp::Reverse(hits));
    log::info!("profile-ra: {} samples", critical.len());
    for (ra, hits) in by_caller.iter().take(top) {
        log::info!("profile-ra: {hits} 0x{ra:08x}");
    }
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
