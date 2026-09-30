//! Open at login, through `SMAppService.mainAppService`: a standard login
//! item the user can see and remove in System Settings. No LaunchAgent, no
//! helper, no privileges. Scheduled runs need diri running to fire, so this
//! is what lets a run due during a restart catch up.

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject, Bool};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginItemStatus {
    Enabled,
    Disabled,
    /// Registered, but the user switched it off in System Settings.
    RequiresApproval,
    /// Not a bundled app (a dev build) or the service is missing.
    Unavailable,
}

fn service() -> Option<*mut AnyObject> {
    let class = AnyClass::get(c"SMAppService")?;
    // SAFETY: `mainAppService` is a class property returning an autoreleased
    // SMAppService that stays valid for the current autorelease scope.
    let service: *mut AnyObject = unsafe { msg_send![class, mainAppService] };
    (!service.is_null()).then_some(service)
}

pub(crate) fn status() -> LoginItemStatus {
    let Some(service) = service() else {
        return LoginItemStatus::Unavailable;
    };
    // SAFETY: `status` is an NSInteger-valued property of SMAppService.
    let status: isize = unsafe { msg_send![service, status] };
    match status {
        1 => LoginItemStatus::Enabled,
        0 => LoginItemStatus::Disabled,
        2 => LoginItemStatus::RequiresApproval,
        _ => LoginItemStatus::Unavailable,
    }
}

/// Registers or unregisters diri as a login item. Returns the new status.
pub(crate) fn set_enabled(enabled: bool) -> Result<LoginItemStatus, String> {
    let service = service().ok_or_else(|| "Login items aren't available here.".to_owned())?;
    let mut error: *mut AnyObject = std::ptr::null_mut();
    // SAFETY: both selectors take an `NSError **` out-parameter and return BOOL.
    let ok: Bool = unsafe {
        if enabled {
            msg_send![service, registerAndReturnError: &mut error]
        } else {
            msg_send![service, unregisterAndReturnError: &mut error]
        }
    };
    let now = status();
    if ok.as_bool() || (enabled && now == LoginItemStatus::RequiresApproval) {
        return Ok(now);
    }
    Err(if error.is_null() {
        "macOS didn't accept the change.".to_owned()
    } else {
        // SAFETY: a non-null out-parameter is an NSError.
        let description: *mut AnyObject = unsafe { msg_send![error, localizedDescription] };
        nsstring(description).unwrap_or_else(|| "macOS didn't accept the change.".to_owned())
    })
}

/// Opens System Settings > General > Login Items.
pub(crate) fn open_settings() {
    if let Some(class) = AnyClass::get(c"SMAppService") {
        // SAFETY: a class method with no arguments and no return value.
        let _: () = unsafe { msg_send![class, openSystemSettingsLoginItems] };
    }
}

fn nsstring(string: *mut AnyObject) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: `UTF8String` returns a NUL-terminated buffer owned by `string`.
    let utf8: *const std::ffi::c_char = unsafe { msg_send![string, UTF8String] };
    (!utf8.is_null()).then(|| {
        unsafe { std::ffi::CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned()
    })
}
