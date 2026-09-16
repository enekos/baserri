use std::io::{self, Write};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct Out {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    pub fn last_line(&self) -> String {
        let pick = if self.out.trim().is_empty() { &self.err } else { &self.out };
        pick.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string()
    }

    pub fn joined(&self) -> String {
        let mut s = self.out.clone();
        if !self.err.trim().is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&self.err);
        }
        s
    }
}

pub fn cmd(prog: &str, args: &[&str]) -> io::Result<Out> {
    cmd_stdin(prog, args, None)
}

pub fn cmd_env(prog: &str, args: &[&str], env: &[(&str, &str)]) -> io::Result<Out> {
    let mut c = Command::new(prog);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let done = c.spawn()?.wait_with_output()?;
    Ok(Out {
        code: done.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&done.stdout).to_string(),
        err: String::from_utf8_lossy(&done.stderr).to_string(),
    })
}

pub fn cmd_stdin(prog: &str, args: &[&str], stdin: Option<&str>) -> io::Result<Out> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("stdin was not piped"))?
            .write_all(text.as_bytes())?;
    }
    let done = child.wait_with_output()?;
    Ok(Out {
        code: done.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&done.stdout).to_string(),
        err: String::from_utf8_lossy(&done.stderr).to_string(),
    })
}

pub fn which(prog: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {prog} >/dev/null 2>&1"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_streams_and_code() {
        let o = cmd("sh", &["-c", "echo out; echo err >&2; exit 3"]).unwrap();
        assert_eq!(o.code, 3);
        assert!(!o.ok());
        assert_eq!(o.out.trim(), "out");
        assert_eq!(o.err.trim(), "err");
    }

    #[test]
    fn stdin_reaches_the_child() {
        let o = cmd_stdin("sh", &["-s"], Some("echo piped-in\n")).unwrap();
        assert_eq!(o.out.trim(), "piped-in");
    }

    #[test]
    fn last_line_falls_back_to_stderr() {
        let o = cmd("sh", &["-c", "echo reason >&2; exit 10"]).unwrap();
        assert_eq!(o.last_line(), "reason");
    }

    #[test]
    fn env_reaches_the_child() {
        let o = cmd_env("sh", &["-c", "echo $BASERRI_TEST_VAR"], &[("BASERRI_TEST_VAR", "set")]).unwrap();
        assert_eq!(o.out.trim(), "set");
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh"));
        assert!(!which("definitely-not-a-real-binary-xyz"));
    }
}
