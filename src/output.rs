use colored::Colorize;

pub fn banner(session_name: &str, verify_cmd: Option<&str>) {
    let ts = chrono_now();
    println!(
        "{}",
        "╭─ claude-run ──────────────────────────────────────╮".cyan()
    );
    println!(
        "{} {} {:<40} {}",
        "│".cyan(),
        "Session:".bold(),
        session_name,
        "│".cyan()
    );
    println!(
        "{} {:<49} {}",
        "│".cyan(),
        format!("Started: {ts}").dimmed(),
        "│".cyan()
    );
    if let Some(cmd) = verify_cmd {
        println!(
            "{} {} {:<40} {}",
            "│".cyan(),
            "Verify:".magenta(),
            cmd,
            "│".cyan()
        );
    }
    println!(
        "{}",
        "╰──────────────────────────────────────────────────╯".cyan()
    );
    println!();
}

pub fn rate_limited(attempt: u32, max: u32, delay_secs: u64) {
    println!();
    println!(
        "{} Retry {}/{} in {delay_secs}s...",
        "Rate limited.".yellow(),
        format!("{attempt}/{max}").bold(),
        max
    );
}

pub fn resuming(session_name: &str) {
    println!(
        "{} session \"{}\"...",
        "Resuming".cyan(),
        session_name.bold()
    );
    println!();
}

pub fn verify_round(round: u32, max: u32, cmd: &str) {
    println!();
    println!(
        "{} (round {round}/{max}): {}",
        "Verifying".magenta().bold(),
        cmd.dimmed()
    );
    println!();
}

pub fn verify_passed() {
    println!();
    println!("{}", "Verification passed.".green().bold());
}

pub fn verify_failed(exit_code: i32) {
    println!();
    println!(
        "{} (exit {exit_code}). Sending Claude back in...",
        "Verification failed".yellow().bold(),
    );
}

pub fn verify_exhausted(max: u32) {
    println!();
    eprintln!(
        "{}",
        format!("Verification still failing after {max} rounds.")
            .red()
            .bold()
    );
}

pub fn claude_error(exit_code: i32) {
    println!();
    eprintln!("{}", format!("Claude exited with code {exit_code}.").red());
}

pub fn daily_cap_waiting(max_retries: u32, poll_secs: u64, timeout_secs: u64) {
    println!();
    println!(
        "{}",
        format!("Still rate-limited after {max_retries} retries — likely a daily cap.")
            .yellow()
            .bold()
    );
    println!("Polling every {poll_secs}s (timeout: {timeout_secs}s)...");
}

pub fn daily_cap_probe(waited_secs: u64) {
    let h = waited_secs / 3600;
    let m = (waited_secs % 3600) / 60;
    println!("{}", format!("Waited {h}h{m}m. Probing...").dimmed());
}

pub fn daily_cap_lifted() {
    println!("{}", "Cap lifted! Resuming session...".green().bold());
}

pub fn done(session_name: &str) {
    println!();
    println!(
        "{} Session: {}",
        "Done.".green().bold(),
        session_name.bold()
    );
}

fn chrono_now() -> String {
    // Simple timestamp without pulling in chrono crate
    use std::process::Command;
    Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}
