#![no_std]
#![no_main]

use esp_hal::main;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[main]
fn main() -> ! {
    let _peripherals = esp_hal::init(esp_hal::Config::default());

    loop {
        core::hint::spin_loop();
    }
}
