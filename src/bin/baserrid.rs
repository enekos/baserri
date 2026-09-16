use baserri::alerts::{self, Thresholds, Watch};
use baserri::bot::{Bot, Reply};
use baserri::conf::Conf;
use baserri::exec::{self, Limits};
use baserri::facts::Facts;
use baserri::run;
use baserri::telegram::Api;
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "/etc/baserri/baserrid.conf".to_string());

    match start(Path::new(&path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("baserrid: {e}");
            ExitCode::FAILURE
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn start(path: &Path) -> Result<(), String> {
    let conf = Conf::load(path)?;
    let token = conf.req("token")?;
    let mut bot = Bot::from_conf(&conf)?;
    let api = Api::new(token);
    let home = *bot.allow.first().ok_or("no allowed chat")?;

    if !run::which("curl") {
        return Err("curl is not installed — it is the only way this talks to Telegram".into());
    }
    if !run::which("timeout") {
        return Err("coreutils `timeout` is missing — commands could not be bounded".into());
    }

    let limits = Limits {
        timeout_s: conf.num("timeout_s", 60),
        max_kb: conf.num("max_kb", 2048),
    };
    let poll_timeout = conf.num("poll_timeout_s", 25);

    let mut offset = drain(&api);
    let facts = Facts::local().unwrap_or_default();
    let _ = api.send(
        home,
        &format!(
            "baserri up — {} on {}, {} MB RAM, root {}% used",
            facts.get("os"),
            facts.get("model"),
            facts.get("mem_mb"),
            facts.get("root_used_pct")
        ),
    );

    spawn_alerts(api.clone(), home, Thresholds::from_conf(&conf));

    let mut backoff = 1;
    loop {
        match api.get_updates(offset, poll_timeout) {
            Err(e) => {
                eprintln!("poll failed: {e}");
                thread::sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(60);
            }
            Ok(updates) => {
                backoff = 1;
                for up in updates {
                    offset = up.id + 1;
                    if !bot.authorized(up.chat) {
                        eprintln!("ignored chat {} ({})", up.chat, up.name);
                        continue;
                    }
                    let reply = bot.respond(&up, now());
                    if let Err(e) = act(&api, up.chat, reply, &limits) {
                        eprintln!("reply failed: {e}");
                    }
                }
            }
        }
    }
}

fn drain(api: &Api) -> i64 {
    match api.get_updates(-1, 0) {
        Ok(updates) => updates.last().map(|u| u.id + 1).unwrap_or(0),
        Err(_) => 0,
    }
}

fn act(api: &Api, chat: i64, reply: Reply, limits: &Limits) -> Result<(), String> {
    match reply {
        Reply::Silent => Ok(()),
        Reply::Text(t) => api.send(chat, &t),
        Reply::Exec { title, command } => {
            let out = exec::shell(&command, limits);
            let heading = if out.code == 0 { title } else { format!("{title} — exit {}", out.code) };
            api.send_block(chat, &heading, &out.text)
        }
        Reply::Reboot => {
            api.send(chat, "rebooting — back in a minute")?;
            let out = exec::shell("sudo -n reboot", limits);
            if out.code != 0 {
                return api.send_block(chat, "reboot failed", &out.text);
            }
            Ok(())
        }
    }
}

fn spawn_alerts(api: Api, chat: i64, t: Thresholds) {
    thread::spawn(move || {
        let mut watch = Watch::default();
        loop {
            thread::sleep(Duration::from_secs(t.interval_s.max(30)));
            let Ok(facts) = Facts::local() else { continue };
            let dead = dead_units(&t.units);
            for message in watch.step(alerts::evaluate(&facts, &t, &dead)) {
                let _ = api.send(chat, &message);
            }
        }
    });
}

fn dead_units(units: &[String]) -> Vec<String> {
    units
        .iter()
        .filter(|u| exec::unit_is_safe(u))
        .filter(|u| {
            !run::cmd("systemctl", &["is-active", "--quiet", u])
                .map(|o| o.ok())
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}
