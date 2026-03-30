use std::time::Duration;

const RATE_LIMIT_PATTERNS: &[&str] = &[
    "rate limit",
    "rate_limit",
    "rate_limit_error",
    "overloaded",
    "too many requests",
    "429",
];

/// Check if a failed claude invocation was rate-limited based on exit code and stderr.
pub fn is_rate_limited(exit_code: i32, stderr: &str) -> bool {
    if exit_code == 0 {
        return false;
    }
    let lower = stderr.to_lowercase();
    RATE_LIMIT_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

/// Exponential backoff with a configurable cap.
#[derive(Debug, Clone)]
pub struct Backoff {
    initial: Duration,
    current: Duration,
    cap: Duration,
}

impl Backoff {
    pub fn new(initial: Duration, cap: Duration) -> Self {
        Self {
            initial,
            current: initial,
            cap,
        }
    }

    /// Return the current delay and advance to the next (doubled, capped).
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.cap);
        delay
    }

    /// Reset to initial delay.
    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_exit_zero_is_false() {
        assert!(!is_rate_limited(0, "rate limit exceeded"));
    }

    #[test]
    fn rate_limited_with_rate_limit_text() {
        assert!(is_rate_limited(1, "Error: rate limit exceeded"));
    }

    #[test]
    fn rate_limited_with_rate_limit_error() {
        assert!(is_rate_limited(1, "rate_limit_error: too fast"));
    }

    #[test]
    fn rate_limited_with_429() {
        assert!(is_rate_limited(1, "HTTP 429 Too Many Requests"));
    }

    #[test]
    fn rate_limited_with_overloaded() {
        assert!(is_rate_limited(1, "API is overloaded"));
    }

    #[test]
    fn rate_limited_case_insensitive() {
        assert!(is_rate_limited(1, "RATE LIMIT exceeded"));
        assert!(is_rate_limited(1, "Rate_Limit_Error"));
    }

    #[test]
    fn not_rate_limited_syntax_error() {
        assert!(!is_rate_limited(1, "syntax error in prompt"));
    }

    #[test]
    fn not_rate_limited_empty_stderr() {
        assert!(!is_rate_limited(1, ""));
    }

    #[test]
    fn backoff_doubles() {
        let mut b = Backoff::new(Duration::from_secs(60), Duration::from_secs(300));
        assert_eq!(b.next_delay(), Duration::from_secs(60));
        assert_eq!(b.next_delay(), Duration::from_secs(120));
        assert_eq!(b.next_delay(), Duration::from_secs(240));
    }

    #[test]
    fn backoff_caps() {
        let mut b = Backoff::new(Duration::from_secs(60), Duration::from_secs(300));
        b.next_delay(); // 60
        b.next_delay(); // 120
        b.next_delay(); // 240
        assert_eq!(b.next_delay(), Duration::from_secs(300)); // capped: 480 -> 300
        assert_eq!(b.next_delay(), Duration::from_secs(300)); // stays capped
    }

    #[test]
    fn backoff_reset() {
        let mut b = Backoff::new(Duration::from_secs(60), Duration::from_secs(300));
        b.next_delay();
        b.next_delay();
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(60));
    }
}
