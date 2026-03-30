use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub max_retries: u32,
    pub retry_delay: Duration,
    pub retry_cap: Duration,
    pub notify: bool,
    pub verify_max: u32,
    pub daily_cap_poll: Duration,
    pub daily_cap_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_retries: 10,
            retry_delay: Duration::from_secs(60),
            retry_cap: Duration::from_secs(300),
            notify: true,
            verify_max: 5,
            daily_cap_poll: Duration::from_secs(300),
            daily_cap_timeout: Duration::from_secs(28800),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Some(v) = parse_env_u64("CLAUDE_MAX_RETRIES") {
            cfg.max_retries = v as u32;
        }
        if let Some(v) = parse_env_u64("CLAUDE_RETRY_DELAY") {
            cfg.retry_delay = Duration::from_secs(v);
        }
        if let Some(v) = parse_env_u64("CLAUDE_RETRY_CAP") {
            cfg.retry_cap = Duration::from_secs(v);
        }
        if let Ok(v) = std::env::var("CLAUDE_NOTIFY") {
            cfg.notify = v != "0";
        }
        if let Some(v) = parse_env_u64("CLAUDE_VERIFY_MAX") {
            cfg.verify_max = v as u32;
        }
        if let Some(v) = parse_env_u64("CLAUDE_DAILY_CAP_POLL") {
            cfg.daily_cap_poll = Duration::from_secs(v);
        }
        if let Some(v) = parse_env_u64("CLAUDE_DAILY_CAP_TIMEOUT") {
            cfg.daily_cap_timeout = Duration::from_secs(v);
        }

        cfg
    }
}

fn parse_env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_bash_script() {
        let cfg = Config::default();
        assert_eq!(cfg.max_retries, 10);
        assert_eq!(cfg.retry_delay, Duration::from_secs(60));
        assert_eq!(cfg.retry_cap, Duration::from_secs(300));
        assert!(cfg.notify);
        assert_eq!(cfg.verify_max, 5);
        assert_eq!(cfg.daily_cap_poll, Duration::from_secs(300));
        assert_eq!(cfg.daily_cap_timeout, Duration::from_secs(28800));
    }

    #[test]
    fn from_env_reads_overrides() {
        // Use unique prefix to avoid test interference
        std::env::set_var("CLAUDE_MAX_RETRIES", "20");
        std::env::set_var("CLAUDE_NOTIFY", "0");
        let cfg = Config::from_env();
        assert_eq!(cfg.max_retries, 20);
        assert!(!cfg.notify);
        // Clean up
        std::env::remove_var("CLAUDE_MAX_RETRIES");
        std::env::remove_var("CLAUDE_NOTIFY");
    }

    #[test]
    fn from_env_ignores_invalid_values() {
        std::env::set_var("CLAUDE_MAX_RETRIES", "not_a_number");
        let cfg = Config::from_env();
        assert_eq!(cfg.max_retries, 10); // falls back to default
        std::env::remove_var("CLAUDE_MAX_RETRIES");
    }
}
