/// Send a macOS notification. No-op on other platforms or if disabled.
pub fn notify(message: &str, enabled: bool) {
    if !enabled {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"claude-run\"",
            message.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output();
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = message; // suppress unused warning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_disabled_does_not_panic() {
        // Should be a no-op
        notify("test message", false);
    }

    #[test]
    fn notify_enabled_does_not_panic() {
        // May or may not show a notification depending on platform
        notify("test message", true);
    }

    #[test]
    fn message_with_special_chars_does_not_panic() {
        notify("test \"quoted\" and back\\slash", true);
    }
}
