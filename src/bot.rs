use crate::conf::Conf;
use crate::exec::{self, Cmd};
use crate::telegram::Update;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Silent,
    Text(String),
    Exec { title: String, command: String },
    Reboot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Shell(String),
    Reboot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub token: String,
    pub action: Action,
    pub chat: i64,
    pub at: u64,
}

pub struct Bot {
    pub allow: Vec<i64>,
    pub jobs: BTreeMap<String, String>,
    pub confirm_ttl: u64,
    pub allow_shell: bool,
    pending: Option<Pending>,
}

pub const HELP: &str = "<b>baserri</b>
/status - load, memory, temperature, throttling, disk
/df - filesystem usage
/jobs - the named jobs in the config
/run &lt;job&gt; - run one of them
/logs &lt;unit&gt; [n] - journal tail
/sh &lt;cmd&gt; - any shell command, as the baserri user, after a confirm
/reboot - after a confirm
/no - drop whatever is waiting for a confirm";

impl Bot {
    pub fn from_conf(c: &Conf) -> Result<Bot, String> {
        let allow: Vec<i64> = c
            .list("allow")
            .iter()
            .map(|v| v.parse::<i64>().map_err(|_| format!("`allow` holds a non-numeric chat id: {v}")))
            .collect::<Result<_, _>>()?;
        if allow.is_empty() {
            return Err("`allow` is empty — the bot would answer nobody".into());
        }
        Ok(Bot {
            allow,
            jobs: c.section("jobs").into_iter().collect(),
            confirm_ttl: c.num("confirm_ttl_s", 90),
            allow_shell: c.flag("allow_shell", true),
            pending: None,
        })
    }

    pub fn authorized(&self, chat: i64) -> bool {
        self.allow.contains(&chat)
    }

    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    pub fn respond(&mut self, up: &Update, now: u64) -> Reply {
        if !self.authorized(up.chat) {
            return Reply::Silent;
        }
        if let Some(p) = &self.pending
            && now.saturating_sub(p.at) > self.confirm_ttl
        {
            self.pending = None;
        }
        match exec::parse(&up.text) {
            Cmd::Empty => Reply::Silent,
            Cmd::Help => Reply::Text(HELP.to_string()),
            Cmd::Status => Reply::Exec { title: "status".into(), command: STATUS.into() },
            Cmd::Disk => Reply::Exec { title: "df".into(), command: "df -h -x tmpfs -x devtmpfs".into() },
            Cmd::Jobs => Reply::Text(self.job_list()),
            Cmd::Run(name) => match self.jobs.get(&name) {
                Some(command) => Reply::Exec { title: format!("job {name}"), command: command.clone() },
                None => Reply::Text(format!("no job called <b>{name}</b>\n\n{}", self.job_list())),
            },
            Cmd::Logs(unit, n) => {
                if exec::unit_is_safe(&unit) {
                    Reply::Exec {
                        title: format!("logs {unit}"),
                        command: format!("journalctl -u {unit} -n {n} --no-pager"),
                    }
                } else {
                    Reply::Text("that is not a unit name".into())
                }
            }
            Cmd::Shell(command) => {
                if !self.allow_shell {
                    return Reply::Text("/sh is disabled in the config".into());
                }
                let token = self.arm(Action::Shell(command.clone()), up.chat, now);
                Reply::Text(format!(
                    "run this?\n<pre>{}</pre>confirm with <code>/yes {token}</code>",
                    crate::telegram::escape_html(&command)
                ))
            }
            Cmd::Reboot => {
                let token = self.arm(Action::Reboot, up.chat, now);
                Reply::Text(format!("reboot the box? confirm with <code>/yes {token}</code>"))
            }
            Cmd::Cancel => {
                let had = self.pending.take().is_some();
                Reply::Text(if had { "dropped".into() } else { "nothing was waiting".into() })
            }
            Cmd::Confirm(token) => match self.pending.take() {
                None => Reply::Text("nothing is waiting for a confirm".into()),
                Some(p) if p.token != token || p.chat != up.chat => {
                    self.pending = Some(p);
                    Reply::Text("that token does not match".into())
                }
                Some(p) => match p.action {
                    Action::Reboot => Reply::Reboot,
                    Action::Shell(command) => Reply::Exec { title: "sh".into(), command },
                },
            },
            Cmd::Unknown(what) => Reply::Text(format!(
                "not a command: <code>{}</code>\n\n{HELP}",
                crate::telegram::escape_html(&what)
            )),
        }
    }

    fn arm(&mut self, action: Action, chat: i64, now: u64) -> String {
        let token = new_token();
        self.pending = Some(Pending { token: token.clone(), action, chat, at: now });
        token
    }

    fn job_list(&self) -> String {
        if self.jobs.is_empty() {
            return "no jobs configured".into();
        }
        let mut s = String::from("<b>jobs</b>\n");
        for (name, command) in &self.jobs {
            s.push_str(&format!(
                "/run {name} - <code>{}</code>\n",
                crate::telegram::escape_html(command)
            ));
        }
        s
    }
}

pub const STATUS: &str = r#"
printf 'host    %s\n' "$(hostname)"
printf 'uptime %s\n' "$(uptime -p 2>/dev/null || true)"
printf 'load    %s\n' "$(cut -d' ' -f1-3 /proc/loadavg)"
free -h | awk 'NR==1||/Mem|Swap/'
if command -v vcgencmd >/dev/null 2>&1; then
  printf 'temp    %s\n' "$(vcgencmd measure_temp | cut -d= -f2)"
  printf 'throttled %s\n' "$(vcgencmd get_throttled | cut -d= -f2)"
fi
df -h / | tail -1 | awk '{print "root    "$3" used of "$2" ("$5")"}'
if command -v docker >/dev/null 2>&1; then
  printf 'docker  %s containers up\n' "$(docker ps -q 2>/dev/null | wc -l | tr -d ' ')"
fi
"#;

pub fn new_token() -> String {
    let mut buf = [0u8; 3];
    let drawn = fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    match drawn {
        true => buf.iter().map(|x| format!("{x:02x}")).collect(),
        false => {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            format!("{:06x}", n & 0xffffff)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot() -> Bot {
        Bot::from_conf(
            &Conf::parse("allow = 42\n[jobs]\nbackup = restic backup /var\n").unwrap(),
        )
        .unwrap()
    }

    fn msg(chat: i64, text: &str) -> Update {
        Update { id: 1, chat, from: chat, name: "eneko".into(), text: text.into() }
    }

    #[test]
    fn a_stranger_gets_nothing_back() {
        let mut b = bot();
        assert_eq!(b.respond(&msg(9999, "/status"), 0), Reply::Silent);
        assert_eq!(b.respond(&msg(9999, "/sh id"), 0), Reply::Silent);
        assert!(b.pending().is_none());
    }

    #[test]
    fn an_empty_allowlist_refuses_to_start() {
        assert!(Bot::from_conf(&Conf::parse("allow =\n").unwrap()).is_err());
        assert!(Bot::from_conf(&Conf::parse("allow = eneko\n").unwrap()).is_err());
    }

    #[test]
    fn sh_never_runs_without_a_confirm() {
        let mut b = bot();
        let r = b.respond(&msg(42, "/sh rm -rf /tmp/x"), 100);
        let Reply::Text(t) = &r else { panic!("expected a confirm prompt, got {r:?}") };
        assert!(t.contains("/yes "));
        let token = b.pending().unwrap().token.clone();
        assert_eq!(b.pending().unwrap().action, Action::Shell("rm -rf /tmp/x".into()));

        assert_eq!(
            b.respond(&msg(42, &format!("/yes {token}")), 110),
            Reply::Exec { title: "sh".into(), command: "rm -rf /tmp/x".into() }
        );
        assert!(b.pending().is_none());
    }

    #[test]
    fn a_confirm_expires() {
        let mut b = bot();
        b.respond(&msg(42, "/sh id"), 0);
        let token = b.pending().unwrap().token.clone();
        let r = b.respond(&msg(42, &format!("/yes {token}")), 1000);
        assert_eq!(r, Reply::Text("nothing is waiting for a confirm".into()));
    }

    #[test]
    fn a_wrong_token_keeps_the_pending_command_armed() {
        let mut b = bot();
        b.respond(&msg(42, "/sh id"), 0);
        assert_eq!(b.respond(&msg(42, "/yes 000000"), 1), Reply::Text("that token does not match".into()));
        assert!(b.pending().is_some());
    }

    #[test]
    fn a_confirm_from_another_chat_does_not_land() {
        let mut b = Bot::from_conf(&Conf::parse("allow = 42, 43\n").unwrap()).unwrap();
        b.respond(&msg(42, "/sh id"), 0);
        let token = b.pending().unwrap().token.clone();
        assert_eq!(
            b.respond(&msg(43, &format!("/yes {token}")), 1),
            Reply::Text("that token does not match".into())
        );
        assert!(b.pending().is_some());
    }

    #[test]
    fn cancel_clears_the_pending_command() {
        let mut b = bot();
        b.respond(&msg(42, "/reboot"), 0);
        assert_eq!(b.pending().unwrap().action, Action::Reboot);
        assert_eq!(b.respond(&msg(42, "/no"), 1), Reply::Text("dropped".into()));
        assert!(b.pending().is_none());
    }

    #[test]
    fn reboot_also_needs_a_confirm() {
        let mut b = bot();
        b.respond(&msg(42, "/reboot"), 0);
        let token = b.pending().unwrap().token.clone();
        assert_eq!(b.respond(&msg(42, &format!("/yes {token}")), 1), Reply::Reboot);
    }

    #[test]
    fn shell_can_be_switched_off_entirely() {
        let mut b = Bot::from_conf(&Conf::parse("allow = 42\nallow_shell = false\n").unwrap()).unwrap();
        assert_eq!(b.respond(&msg(42, "/sh id"), 0), Reply::Text("/sh is disabled in the config".into()));
        assert!(b.pending().is_none());
    }

    #[test]
    fn jobs_run_without_a_confirm_because_you_wrote_them() {
        let mut b = bot();
        assert_eq!(
            b.respond(&msg(42, "/run backup"), 0),
            Reply::Exec { title: "job backup".into(), command: "restic backup /var".into() }
        );
    }

    #[test]
    fn an_unknown_job_lists_the_real_ones() {
        let mut b = bot();
        let Reply::Text(t) = b.respond(&msg(42, "/run nope"), 0) else { panic!() };
        assert!(t.contains("no job called"));
        assert!(t.contains("/run backup"));
    }

    #[test]
    fn a_log_unit_cannot_smuggle_shell() {
        let mut b = bot();
        assert_eq!(
            b.respond(&msg(42, "/logs a;id"), 0),
            Reply::Text("that is not a unit name".into())
        );
    }

    #[test]
    fn tokens_are_six_hex_characters() {
        let t = new_token();
        assert_eq!(t.len(), 6);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_token(), new_token());
    }
}
