//! A minimal macOS `NSApplication` subclass conforming to the Objective-C protocols CEF requires
//! there (issue #23 round 1, round 2 diagnosis Q3b).
//!
//! CEF's own macOS message-pump integration expects `[NSApplication sharedApplication]` to
//! conform to `CrAppProtocol`/`CrAppControlProtocol` (`cef::application_mac::CefAppProtocol`).
//! Every cef-rs macOS example (`examples/cefsimple/src/mac` in
//! <https://github.com/tauri-apps/cef-rs>) installs this before calling `cef::initialize`, for
//! every process type (browser, renderer, GPU), even though nothing else in CEF's `Settings` or
//! command-line flags mentions it. Round 1's first macOS run skipped this: CEF started, the
//! helper connected and created the page's browser, then the whole process ended a couple of
//! seconds later with no panic, no CEF log line and no crash report content to explain why,
//! consistent with `NSApp` not answering `isHandlingSendEvent`/`setHandlingSendEvent:` the way
//! CEF's internal Cocoa event-pump integration expects once it starts doing real work for a
//! loading page.
//!
//! This only implements the required protocol methods and hides the process from the Dock and
//! the application switcher (`NSApplicationActivationPolicy::Prohibited`, round 2 diagnosis Q4:
//! cod-browser never shows a window of its own, so it has no reason to appear as a second app).
//! It skips the menu bar, Nib loading and application delegate the upstream GUI example also
//! wires up: none of that applies to a windowless helper that Cod itself starts and stops.

use std::cell::Cell;

use cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use objc2::{DefinedClass, define_class, extern_methods, msg_send, rc::Retained, runtime::Bool};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent};

/// Instance variables of `CodBrowserApplication`, matching cef-rs's own macOS example.
#[derive(Default)]
pub struct CodBrowserApplicationIvars {
    handling_send_event: Cell<Bool>,
}

define_class!(
    /// An `NSApplication` subclass that implements the protocols CEF requires on macOS.
    #[unsafe(super(NSApplication))]
    #[ivars = CodBrowserApplicationIvars]
    pub struct CodBrowserApplication;

    impl CodBrowserApplication {
        #[unsafe(method(sendEvent:))]
        unsafe fn send_event(&self, event: &NSEvent) {
            let was_sending_event = self.is_handling_send_event();
            if !was_sending_event {
                self.set_handling_send_event(true);
            }
            let _: () = msg_send![super(self), sendEvent: event];
            if !was_sending_event {
                self.set_handling_send_event(false);
            }
        }
    }

    unsafe impl CrAppControlProtocol for CodBrowserApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        unsafe fn _set_handling_send_event(&self, handling_send_event: Bool) {
            self.ivars().handling_send_event.set(handling_send_event);
        }
    }

    unsafe impl CrAppProtocol for CodBrowserApplication {
        #[unsafe(method(isHandlingSendEvent))]
        unsafe fn _is_handling_send_event(&self) -> Bool {
            self.ivars().handling_send_event.get()
        }
    }

    unsafe impl CefAppProtocol for CodBrowserApplication {}
);

impl CodBrowserApplication {
    extern_methods! {
        #[unsafe(method(sharedApplication))]
        fn shared_application() -> Retained<Self>;

        #[unsafe(method(setHandlingSendEvent:))]
        fn set_handling_send_event(&self, handling_send_event: bool);

        #[unsafe(method(isHandlingSendEvent))]
        fn is_handling_send_event(&self) -> bool;
    }
}

/// Installs `CodBrowserApplication` as `NSApp` and hides it from the Dock and the application
/// switcher. Must run before any other CEF call, in particular before `cef::initialize`, the same
/// as every cef-rs macOS example; `engine::main` calls this right after loading the framework,
/// for every process type (it is harmless, and already what upstream examples do unconditionally).
pub fn setup() {
    let app = CodBrowserApplication::shared_application();
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
}
