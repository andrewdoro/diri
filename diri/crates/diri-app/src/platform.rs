//! User-facing desktop vocabulary that genuinely varies by operating system.

pub fn local_machine_label() -> &'static str {
    crate::i18n::t(if cfg!(target_os = "macos") {
        "platform.this_mac"
    } else {
        "platform.this_computer"
    })
}

pub fn local_machine_label_lowercase() -> &'static str {
    crate::i18n::t(if cfg!(target_os = "macos") {
        "platform.this_mac_lowercase"
    } else {
        "platform.this_computer_lowercase"
    })
}
