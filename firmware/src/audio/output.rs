//! Where the player's sound goes: the USB speaker if one is connected,
//! otherwise the built-in speaker, so an alarm is audible without the
//! Pebble. Both take their samples from the same player, so only one of
//! them streams at a time.
//!
//! [`Output::update`] follows the USB speaker coming and going (called
//! once a second by the alarm clock): a Pebble unplugged while an alarm
//! rings hands the sound to the built-in speaker within a second, and back
//! when it is plugged in again.

use super::builtin::BuiltinSpeaker;
use super::uac::UsbAudio;

/// Which output plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Silent,
    Usb,
    Builtin,
}

pub struct Output {
    usb: UsbAudio,
    builtin: BuiltinSpeaker,
    /// Sound is wanted (an alarm rings or a preview plays).
    wanted: bool,
    route: Route,
}

impl Output {
    pub fn new(usb: UsbAudio, builtin: BuiltinSpeaker) -> Self {
        Self {
            usb,
            builtin,
            wanted: false,
            route: Route::Silent,
        }
    }

    /// Starts or stops the sound, on whichever speaker is there.
    pub fn set_streaming(&mut self, on: bool) {
        self.wanted = on;
        self.update();
    }

    /// True if the sound goes to the built-in speaker.
    pub fn on_builtin(&self) -> bool {
        self.route == Route::Builtin
    }

    /// Re-routes after the USB speaker came or went.
    pub fn update(&mut self) {
        let route = match (self.wanted, self.usb.connected()) {
            (false, _) => Route::Silent,
            (true, true) => Route::Usb,
            (true, false) => Route::Builtin,
        };
        if route == self.route {
            return;
        }
        log::info!("output: {:?} → {route:?}", self.route);
        // Off before on: the two must not take from the player together.
        if self.route == Route::Usb {
            self.usb.set_streaming(false);
        }
        if self.route == Route::Builtin {
            self.builtin.set_streaming(false);
        }
        match route {
            Route::Usb => self.usb.set_streaming(true),
            Route::Builtin => self.builtin.set_streaming(true),
            Route::Silent => {}
        }
        self.route = route;
    }
}
