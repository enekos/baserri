use crate::run;
use crate::ssh::Host;
use std::path::PathBuf;

pub const NEEDS_WORK: i32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Done(String),
    Todo(String),
    Blocked(String),
}

impl State {
    pub fn mark(&self) -> &'static str {
        match self {
            State::Done(_) => "ok",
            State::Todo(_) => "todo",
            State::Blocked(_) => "blocked",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            State::Done(m) | State::Todo(m) | State::Blocked(m) => m,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Shell {
    pub name: String,
    pub why: String,
    pub check: String,
    pub apply: String,
    pub root: bool,
}

#[derive(Debug, Clone)]
pub struct Upload {
    pub name: String,
    pub why: String,
    pub local: PathBuf,
    pub remote: String,
    pub mode: String,
    pub post: String,
}

#[derive(Debug, Clone)]
pub enum Step {
    Shell(Shell),
    Upload(Upload),
    Manual { name: String, why: String, check: String, instruction: String },
}

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::Shell(s) => &s.name,
            Step::Upload(u) => &u.name,
            Step::Manual { name, .. } => name,
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Step::Shell(s) => &s.why,
            Step::Upload(u) => &u.why,
            Step::Manual { why, .. } => why,
        }
    }

    pub fn check(&self, host: &Host) -> State {
        match self {
            Step::Shell(s) => classify(host, &s.check, s.root),
            Step::Manual { check, .. } => match classify(host, check, false) {
                State::Todo(m) => State::Blocked(m),
                other => other,
            },
            Step::Upload(u) => {
                let Some(local) = sha256_local(&u.local) else {
                    return State::Blocked(format!("{} is missing — build it first", u.local.display()));
                };
                let script = format!(
                    "test -f {0} || exit {1}\nremote=$(sha256sum {0} | cut -d' ' -f1)\n[ \"$remote\" = \"{2}\" ] || {{ echo \"installed copy differs\"; exit {1}; }}\necho \"{2}\"",
                    u.remote, NEEDS_WORK, local
                );
                classify(host, &script, true)
            }
        }
    }

    pub fn apply(&self, host: &Host) -> Result<String, String> {
        match self {
            Step::Shell(s) => {
                let out = exec(host, &s.apply, s.root).map_err(|e| e.to_string())?;
                if out.ok() {
                    Ok(out.last_line())
                } else {
                    Err(out.joined().trim().to_string())
                }
            }
            Step::Manual { instruction, .. } => Err(format!("needs you: {instruction}")),
            Step::Upload(u) => {
                let staged = format!("/tmp/arola-upload-{}", u.name);
                let put = host.put(&u.local, &staged).map_err(|e| e.to_string())?;
                if !put.ok() {
                    return Err(format!("scp failed: {}", put.joined().trim()));
                }
                let script = format!(
                    "install -D -m {} {} {}\nrm -f {}\n{}",
                    u.mode, staged, u.remote, staged, u.post
                );
                let out = exec(host, &script, true).map_err(|e| e.to_string())?;
                if out.ok() {
                    Ok(format!("installed {}", u.remote))
                } else {
                    Err(out.joined().trim().to_string())
                }
            }
        }
    }
}

fn exec(host: &Host, script: &str, root: bool) -> std::io::Result<run::Out> {
    if root { host.sudo(script) } else { host.sh(script) }
}

fn classify(host: &Host, script: &str, root: bool) -> State {
    match exec(host, script, root) {
        Err(e) => State::Blocked(format!("could not reach the host: {e}")),
        Ok(o) if o.ok() => State::Done(o.last_line()),
        Ok(o) if o.code == NEEDS_WORK => State::Todo(o.last_line()),
        Ok(o) => State::Blocked(format!("check exited {}: {}", o.code, o.last_line())),
    }
}

pub fn sha256_local(path: &std::path::Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let p = path.display().to_string();
    for (prog, args) in [("shasum", vec!["-a", "256"]), ("sha256sum", vec![])] {
        let mut a = args.clone();
        a.push(&p);
        if let Ok(o) = run::cmd(prog, &a)
            && o.ok()
            && let Some(hash) = o.out.split_whitespace().next()
        {
            return Some(hash.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn state_marks_are_stable() {
        assert_eq!(State::Done(String::new()).mark(), "ok");
        assert_eq!(State::Todo(String::new()).mark(), "todo");
        assert_eq!(State::Blocked(String::new()).mark(), "blocked");
    }

    #[test]
    fn sha256_matches_the_shell() {
        let mut f = std::env::temp_dir();
        f.push(format!("arola-sha-test-{}", std::process::id()));
        std::fs::File::create(&f).unwrap().write_all(b"arola").unwrap();
        let got = sha256_local(&f).unwrap();
        let want = run::cmd("shasum", &["-a", "256", &f.display().to_string()]).unwrap();
        std::fs::remove_file(&f).ok();
        assert_eq!(want.out.split_whitespace().next().unwrap(), got);
        assert_eq!(got.len(), 64);
    }

    #[test]
    fn a_missing_file_has_no_hash() {
        assert!(sha256_local(std::path::Path::new("/nope/arola/missing")).is_none());
    }
}
