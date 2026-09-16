use baserri::alerts::{self, Thresholds, Watch};
use baserri::bot::{Bot, Reply};
use baserri::cleanup;
use baserri::conf::Conf;
use baserri::exec::{self, Limits};
use baserri::facts::Facts;
use baserri::github;
use baserri::run;
use baserri::telegram::Api;
use baserri::watch::{Change, Transitions};
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Ctx {
    api: Api,
    limits: Limits,
    github: Option<github::Client>,
}

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
    let home = *bot.allow.first().ok_or("no allowed chat")?;

    if !run::which("curl") {
        return Err("curl is not installed — it is the only way this talks to Telegram".into());
    }
    if !run::which("timeout") {
        return Err("coreutils `timeout` is missing — commands could not be bounded".into());
    }

    let ctx = Ctx {
        api: Api::new(token),
        limits: Limits {
            timeout_s: conf.num("timeout_s", 60),
            max_kb: conf.num("max_kb", 2048),
        },
        github: conf.get("github.token").filter(|t| !t.is_empty()).map(github::Client::new),
    };
    let poll_timeout = conf.num("poll_timeout_s", 25);

    let mut offset = drain(&ctx.api);
    let facts = Facts::local().unwrap_or_default();
    let watching = match &ctx.github {
        Some(_) => "watching github",
        None => "no github token, PR watch off",
    };
    let _ = ctx.api.send(
        home,
        &format!(
            "baserri up — {} on {}, {} MB RAM, root {}% used\n{watching}",
            facts.get("os"),
            facts.get("model"),
            facts.get("mem_mb"),
            facts.get("root_used_pct")
        ),
    );

    spawn_alerts(ctx.api.clone(), home, Thresholds::from_conf(&conf));
    if let Some(gh_token) = conf.get("github.token").filter(|t| !t.is_empty()) {
        spawn_pr_watch(ctx.api.clone(), home, gh_token.to_string(), conf.num("github.poll_s", 300));
    }
    spawn_disk_watch(ctx.api.clone(), home, &conf);

    let mut backoff = 1;
    loop {
        match ctx.api.get_updates(offset, poll_timeout) {
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
                    if let Err(e) = act(&ctx, up.chat, reply) {
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

fn sweep(apply: bool, limits: &Limits) -> String {
    let flag = if apply { " --apply" } else { "" };
    let out = exec::shell(&format!("sudo -n {} all{flag}", cleanup::HELPER), limits);
    if out.code != 0 && out.text.contains("sudo") {
        return format!(
            "the sweep helper is not installed or not allowed: <pre>{}</pre>run `baserri apply --only cleanup` from your laptop",
            baserri::telegram::escape_html(out.text.trim())
        );
    }
    cleanup::format_report(&cleanup::parse_report(&out.text), apply)
}

fn act(ctx: &Ctx, chat: i64, reply: Reply) -> Result<(), String> {
    match reply {
        Reply::Silent => Ok(()),
        Reply::Text(t) => ctx.api.send(chat, &t),
        Reply::Exec { title, command } => {
            let out = exec::shell(&command, &ctx.limits);
            let heading = if out.code == 0 { title } else { format!("{title} — exit {}", out.code) };
            ctx.api.send_block(chat, &heading, &out.text)
        }
        Reply::Cleanup { apply } => ctx.api.send(chat, &sweep(apply, &ctx.limits)),
        Reply::Prs => match &ctx.github {
            None => ctx.api.send(chat, "no github token in the config"),
            Some(client) => match client.open_prs() {
                Ok(prs) => ctx.api.send(chat, &github::board(&prs)),
                Err(e) => ctx.api.send(chat, &format!("github: {e}")),
            },
        },
        Reply::Reboot => {
            ctx.api.send(chat, "rebooting — back in a minute")?;
            let out = exec::shell("sudo -n reboot", &ctx.limits);
            if out.code != 0 {
                return ctx.api.send_block(chat, "reboot failed", &out.text);
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

fn spawn_pr_watch(api: Api, chat: i64, token: String, every: u64) {
    thread::spawn(move || {
        let client = github::Client::new(&token);
        let mut seen = Transitions::default();
        loop {
            match client.open_prs() {
                Err(e) => eprintln!("github poll failed: {e}"),
                Ok(prs) => {
                    let state = prs
                        .iter()
                        .map(|p| (p.key(), p.snapshot.render()))
                        .collect::<Vec<_>>();
                    for change in seen.step(state) {
                        let message = match &change {
                            Change::Gone { key, .. } => Some(format!(
                                "\u{1f3c1} <a href=\"{}\">{}</a> {}",
                                github::url_for(key),
                                baserri::telegram::escape_html(key),
                                client.closed_how(key)
                            )),
                            other => github::describe(other),
                        };
                        if let Some(m) = message {
                            let _ = api.send(chat, &m);
                        }
                    }
                }
            }
            thread::sleep(Duration::from_secs(every.max(60)));
        }
    });
}

fn spawn_disk_watch(api: Api, chat: i64, conf: &Conf) {
    let every = conf.num("cleanup.interval_s", 86400);
    let floor = conf.num("cleanup.notify_gb", 2) * 1024 * 1024 * 1024;
    let limits = Limits { timeout_s: 300, max_kb: 1024 };
    if every == 0 {
        return;
    }
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(every.max(3600)));
            let out = exec::shell(&format!("sudo -n {} all", cleanup::HELPER), &limits);
            let rows = cleanup::parse_report(&out.text);
            let total: u64 = rows.iter().map(|(_, b)| b).sum();
            if total >= floor {
                let report = cleanup::format_report(&rows, false);
                let _ = api.send(chat, &format!("{report}\nsend /cleanup apply to free it"));
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
