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
    pub av_threshold: u32,
    pub av_rounds: u32,
    pub av_model: Option<String>,
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
            av_threshold: 95,
            av_rounds: 3,
            av_model: None,
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
        if let Some(v) = parse_env_u64("CLAUDE_AV_THRESHOLD") {
            cfg.av_threshold = v as u32;
        }
        if let Some(v) = parse_env_u64("CLAUDE_AV_ROUNDS") {
            cfg.av_rounds = v as u32;
        }
        if let Ok(v) = std::env::var("CLAUDE_AV_MODEL") {
            if !v.is_empty() {
                cfg.av_model = Some(v);
            }
        }

        cfg
    }

    /// Get the configured AV rounds (for use from lib.rs pipeline builder).
    pub fn av_rounds(&self) -> u32 {
        self.av_rounds
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
    fn av_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.av_threshold, 95);
        assert_eq!(cfg.av_rounds, 3);
        assert!(cfg.av_model.is_none());
    }

    #[test]
    fn from_env_reads_overrides() {
        std::env::set_var("CLAUDE_MAX_RETRIES", "20");
        std::env::set_var("CLAUDE_NOTIFY", "0");
        let cfg = Config::from_env();
        assert_eq!(cfg.max_retries, 20);
        assert!(!cfg.notify);
        std::env::remove_var("CLAUDE_MAX_RETRIES");
        std::env::remove_var("CLAUDE_NOTIFY");
    }

    #[test]
    fn from_env_ignores_invalid_values() {
        std::env::set_var("CLAUDE_MAX_RETRIES", "not_a_number");
        let cfg = Config::from_env();
        assert_eq!(cfg.max_retries, 10);
        std::env::remove_var("CLAUDE_MAX_RETRIES");
    }

    #[test]
    fn from_env_reads_av_overrides() {
        std::env::set_var("CLAUDE_AV_THRESHOLD", "90");
        std::env::set_var("CLAUDE_AV_ROUNDS", "5");
        let cfg = Config::from_env();
        assert_eq!(cfg.av_threshold, 90);
        assert_eq!(cfg.av_rounds, 5);
        std::env::remove_var("CLAUDE_AV_THRESHOLD");
        std::env::remove_var("CLAUDE_AV_ROUNDS");
    }
}
