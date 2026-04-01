use colored::Colorize;

pub fn banner(
    session_name: &str,
    verify_cmd: Option<&str>,
    av: Option<(&str, u32)>,
    pipeline_file: Option<&str>,
) {
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
    if let Some(path) = pipeline_file {
        println!(
            "{} {} {:<40} {}",
            "│".cyan(),
            "Pipeline:".cyan().bold(),
            path,
            "│".cyan()
        );
    }
    if let Some(cmd) = verify_cmd {
        println!(
            "{} {} {:<40} {}",
            "│".cyan(),
            "Verify:".magenta(),
            cmd,
            "│".cyan()
        );
    }
    if let Some((spec, threshold)) = av {
        println!(
            "{} {} {:<40} {}",
            "│".cyan(),
            "Adversarial:".yellow(),
            format!("{spec} (threshold: {threshold})"),
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
    if max > 0 {
        println!(
            "{} (round {round}/{max}): {}",
            "Verifying".magenta().bold(),
            cmd.dimmed()
        );
    } else {
        println!("{}: {}", "Verifying".magenta().bold(), cmd.dimmed());
    }
    println!();
}

pub fn verify_passed() {
    println!();
    println!("{}", "Verification passed.".green().bold());
}

pub fn verify_failed(exit_code: i32) {
    println!();
    if exit_code != 0 {
        println!(
            "{} (exit {exit_code}). Sending Claude back in...",
            "Verification failed".yellow().bold(),
        );
    } else {
        println!(
            "{}. Sending Claude back in...",
            "Verification failed".yellow().bold(),
        );
    }
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

// ─── Adversarial verification output ───────────────────────────────

pub fn av_round(round: u32, max: u32) {
    println!();
    println!(
        "{} (round {round}/{max})",
        "Adversarial review".yellow().bold(),
    );
}

pub fn av_score(score: u32, threshold: u32, missing: usize, partial: usize, incorrect: usize) {
    let score_str = format!("{score}/100");
    let threshold_str = format!("(threshold: {threshold})");

    if score >= threshold {
        println!(
            "  Score: {} {}",
            score_str.green().bold(),
            threshold_str.dimmed()
        );
    } else {
        println!(
            "  Score: {} {}",
            score_str.yellow().bold(),
            threshold_str.dimmed()
        );
    }

    if missing + partial + incorrect > 0 {
        println!(
            "  Missing: {} | Partial: {} | Incorrect: {}",
            missing, partial, incorrect
        );
    }
}

pub fn av_passed(score: u32) {
    println!();
    println!(
        "{} ({score}/100)",
        "Adversarial review passed.".green().bold()
    );
}

pub fn av_fixing(issues: usize) {
    println!();
    println!(
        "{} Sending worker back to address {issues} issue(s)...",
        "Fix needed.".yellow().bold(),
    );
}

pub fn av_exhausted(score: u32, threshold: u32, max_rounds: u32) {
    println!();
    eprintln!(
        "{}",
        format!(
            "Adversarial review still below threshold ({score}/{threshold}) after {max_rounds} rounds."
        )
        .red()
        .bold()
    );
}

pub fn av_no_verdict() {
    println!();
    println!(
        "{}",
        "Reviewer did not produce a parseable verdict (treating as score 0).".yellow()
    );
}

fn chrono_now() -> String {
    use std::process::Command;
    Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}
