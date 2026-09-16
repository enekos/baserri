use crate::run;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub timeout_s: u64,
    pub max_kb: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { timeout_s: 60, max_kb: 2048 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub code: i32,
    pub text: String,
    pub timed_out: bool,
}

pub const TIMEOUT_CODE: i32 = 124;

pub fn wrap(command: &str, limits: &Limits) -> String {
    format!("ulimit -f {}\nexec 2>&1\n{command}", limits.max_kb)
}

pub fn shell(command: &str, limits: &Limits) -> Outcome {
    let seconds = limits.timeout_s.to_string();
    let script = wrap(command, limits);
    let out = run::cmd("timeout", &["-k", "5", &seconds, "bash", "-lc", &script]);
    match out {
        Err(e) => Outcome { code: -1, text: format!("could not spawn: {e}"), timed_out: false },
        Ok(o) => {
            let timed_out = o.code == TIMEOUT_CODE;
            let mut text = o.joined();
            if timed_out {
                text.push_str(&format!("\n[killed after {seconds}s]"));
            }
            Outcome { code: o.code, text, timed_out }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Help,
    Status,
    Disk,
    Jobs,
    Run(String),
    Logs(String, u32),
    Shell(String),
    Confirm(String),
    Cancel,
    Reboot,
    Prs,
    Cleanup { apply: bool },
    Unknown(String),
    Empty,
}

pub fn parse(text: &str) -> Cmd {
    let text = text.trim();
    let Some(rest) = text.strip_prefix('/') else {
        return if text.is_empty() { Cmd::Empty } else { Cmd::Unknown(text.to_string()) };
    };
    let (head, tail) = match rest.split_once(char::is_whitespace) {
        Some((h, t)) => (h, t.trim()),
        None => (rest, ""),
    };
    let head = head.split('@').next().unwrap_or(head).to_lowercase();
    match head.as_str() {
        "start" | "help" => Cmd::Help,
        "status" => Cmd::Status,
        "df" | "disk" => Cmd::Disk,
        "jobs" => Cmd::Jobs,
        "run" if !tail.is_empty() => Cmd::Run(tail.to_string()),
        "logs" if !tail.is_empty() => {
            let mut parts = tail.split_whitespace();
            let unit = parts.next().unwrap_or_default().to_string();
            let n = parts.next().and_then(|v| v.parse().ok()).unwrap_or(40);
            Cmd::Logs(unit, n.min(200))
        }
        "sh" if !tail.is_empty() => Cmd::Shell(tail.to_string()),
        "yes" | "confirm" if !tail.is_empty() => Cmd::Confirm(tail.to_string()),
        "no" | "cancel" => Cmd::Cancel,
        "reboot" => Cmd::Reboot,
        "prs" | "pr" => Cmd::Prs,
        "cleanup" | "sweep" => Cmd::Cleanup { apply: tail.eq_ignore_ascii_case("apply") },
        other => Cmd::Unknown(other.to_string()),
    }
}

pub fn unit_is_safe(unit: &str) -> bool {
    !unit.is_empty()
        && unit.len() <= 64
        && unit
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_command_surface() {
        assert_eq!(parse("/status"), Cmd::Status);
        assert_eq!(parse("  /df  "), Cmd::Disk);
        assert_eq!(parse("/run backup"), Cmd::Run("backup".into()));
        assert_eq!(parse("/logs baserrid 10"), Cmd::Logs("baserrid".into(), 10));
        assert_eq!(parse("/logs baserrid"), Cmd::Logs("baserrid".into(), 40));
        assert_eq!(parse("/sh df -h /"), Cmd::Shell("df -h /".into()));
        assert_eq!(parse("/yes a1b2c3"), Cmd::Confirm("a1b2c3".into()));
        assert_eq!(parse("/no"), Cmd::Cancel);
        assert_eq!(parse(""), Cmd::Empty);
    }

    #[test]
    fn cleanup_only_sweeps_when_told_to() {
        assert_eq!(parse("/cleanup"), Cmd::Cleanup { apply: false });
        assert_eq!(parse("/cleanup apply"), Cmd::Cleanup { apply: true });
        assert_eq!(parse("/cleanup APPLY"), Cmd::Cleanup { apply: true });
        assert_eq!(parse("/cleanup please"), Cmd::Cleanup { apply: false });
        assert_eq!(parse("/prs"), Cmd::Prs);
    }

    #[test]
    fn a_bare_verb_that_needs_an_argument_is_not_that_verb() {
        assert_eq!(parse("/sh"), Cmd::Unknown("sh".into()));
        assert_eq!(parse("/run"), Cmd::Unknown("run".into()));
        assert_eq!(parse("/yes"), Cmd::Unknown("yes".into()));
    }

    #[test]
    fn group_suffixes_and_case_are_handled() {
        assert_eq!(parse("/Status@baserri_bot"), Cmd::Status);
    }

    #[test]
    fn plain_text_is_never_a_command() {
        assert_eq!(parse("status"), Cmd::Unknown("status".into()));
        assert_eq!(parse("rm -rf /"), Cmd::Unknown("rm -rf /".into()));
    }

    #[test]
    fn the_logs_count_is_capped() {
        assert_eq!(parse("/logs baserrid 99999"), Cmd::Logs("baserrid".into(), 200));
    }

    #[test]
    fn unit_names_reject_shell_metacharacters() {
        assert!(unit_is_safe("baserrid"));
        assert!(unit_is_safe("docker.service"));
        assert!(!unit_is_safe("baserrid; rm -rf /"));
        assert!(!unit_is_safe("$(id)"));
        assert!(!unit_is_safe(""));
    }

    #[test]
    fn a_real_command_runs_and_reports_its_code() {
        let o = shell("echo hello; exit 7", &Limits::default());
        assert_eq!(o.code, 7);
        assert!(o.text.contains("hello"));
        assert!(!o.timed_out);
    }

    #[test]
    fn stderr_is_folded_into_the_reply() {
        let o = shell("echo to-stderr >&2", &Limits::default());
        assert!(o.text.contains("to-stderr"));
    }

    #[test]
    fn a_hang_is_killed_and_labelled() {
        let o = shell("sleep 30", &Limits { timeout_s: 1, max_kb: 64 });
        assert!(o.timed_out);
        assert_eq!(o.code, TIMEOUT_CODE);
        assert!(o.text.contains("killed after 1s"));
    }

    #[test]
    fn the_wrapper_caps_file_writes() {
        assert!(wrap("x", &Limits { timeout_s: 5, max_kb: 128 }).starts_with("ulimit -f 128"));
    }
}
