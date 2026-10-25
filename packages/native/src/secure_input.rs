//! Password display mapping and a balanced macOS secure-event-input lease.
//! GPUI has no secure-input API, so the lease uses the system Carbon API.
pub(crate) fn mask(text: &str) -> String {
    "•".repeat(text.chars().count())
}
pub(crate) fn display_index(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(i, _)| *i < byte).count() * '•'.len_utf8()
}
pub(crate) fn content_index(text: &str, byte: usize) -> usize {
    text.char_indices().nth(byte / '•'.len_utf8()).map(|(i, _)| i).unwrap_or(text.len())
}
#[derive(Default)]
pub(crate) struct Session {
    active: bool,
}
#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
}
impl Session {
    pub(crate) fn set(&mut self, enabled: bool) {
        if self.active == enabled {
            return;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            let result = if enabled { EnableSecureEventInput() } else { DisableSecureEventInput() };
            if result == 0 {
                self.active = enabled;
            }
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.set(false);
    }
}
