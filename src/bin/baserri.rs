use baserri::conf::Conf;
use baserri::facts::Facts;
use baserri::plan::{self, Ctx};
use baserri::json;
use baserri::run;
use baserri::ssh::Host;
use baserri::step::{State, Step};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const TARGET: &str = "aarch64-unknown-linux-musl";
const LINKER_VAR: &str = "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER";

const USAGE: &str = "baserri — provision the home box

  baserri init                  write baserri.conf and baserrid.conf if they are missing
  baserri facts                 what the box says it is
  baserri doctor                facts plus everything that will bite
  baserri plan                  what apply would change
  baserri apply [--only NAME]   converge the box, one step at a time
  baserri build                 cross-compile baserrid for aarch64 musl
  baserri ship                  build, then apply only the baserrid steps
  baserri logs [N]              tail the control plane journal

options
  --config PATH               default ./baserri.conf
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let conf_path = flag_value(&args, "--config").unwrap_or_else(|| "baserri.conf".to_string());
    let only = flag_value(&args, "--only");
    let command = args.first().map(String::as_str).unwrap_or("help");

    let result = match command {
        "init" => init(Path::new(&conf_path)),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        "build" => build().map(|p| println!("built {}", p.display())),
        "facts" => with_host(&conf_path, |_, host| {
            let f = Facts::probe(host)?;
            for (k, v) in f.pairs() {
                println!("{k:<16} {v}");
            }
            Ok(())
        }),
        "doctor" => with_host(&conf_path, |_, host| doctor(host)),
        "plan" => with_ctx(&conf_path, |ctx, host| converge(ctx, host, false, only.as_deref())),
        "apply" => with_ctx(&conf_path, |ctx, host| converge(ctx, host, true, only.as_deref())),
        "ship" => {
            let built = build();
            match built {
                Err(e) => Err(e),
                Ok(_) => with_ctx(&conf_path, |ctx, host| converge(ctx, host, true, Some("baserrid"))),
            }
        }
        "logs" => {
            let n = args.get(1).and_then(|v| v.parse::<u32>().ok()).unwrap_or(50);
            with_host(&conf_path, move |_, host| {
                let out = host
                    .sh(&format!("journalctl -u baserrid -n {n} --no-pager"))
                    .map_err(|e| e.to_string())?;
                print!("{}", out.joined());
                Ok(())
            })
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("baserri: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn load(conf_path: &str) -> Result<Conf, String> {
    let p = Path::new(conf_path);
    if !p.exists() {
        return Err(format!("{conf_path} does not exist — run `baserri init` first"));
    }
    Conf::load(p)
}

fn with_host<F>(conf_path: &str, f: F) -> Result<(), String>
where
    F: FnOnce(&Conf, &Host) -> Result<(), String>,
{
    let conf = load(conf_path)?;
    let host = Host::from_conf(&conf)?;
    host.reachable()?;
    f(&conf, &host)
}

fn with_ctx<F>(conf_path: &str, f: F) -> Result<(), String>
where
    F: FnOnce(&Ctx, &Host) -> Result<(), String>,
{
    let conf = load(conf_path)?;
    let host = Host::from_conf(&conf)?;
    host.reachable()?;
    let facts = Facts::probe(&host)?;
    let ctx = Ctx {
        daemon_binary: match conf.get("daemon_binary") {
            Some(p) => PathBuf::from(p),
            None => artifact()?,
        },
        daemon_config: PathBuf::from(conf.or("daemon_config", "baserrid.conf")),
        conf,
        facts,
    };
    f(&ctx, &host)
}

fn doctor(host: &Host) -> Result<(), String> {
    let f = Facts::probe(host)?;
    println!("{}", f.get("model"));
    println!(
        "{} / {} MB RAM / {} cores / {}",
        f.get("os"),
        f.get("mem_mb"),
        f.get("cpus"),
        f.get("arch")
    );
    println!(
        "root {} on {} — {}% used, {} GB free",
        f.get("root_kind"),
        f.get("root_src"),
        f.get("root_used_pct"),
        f.get("root_avail_gb")
    );
    println!("temp {} C, throttle flags {}", f.get("temp_c"), f.get("throttled"));
    println!(
        "docker {} / tailscale {} / restic {} / baserrid {}",
        f.get("docker"),
        f.get("tailscale"),
        f.get("restic"),
        f.get("baserrid")
    );

    let warnings = f.warnings();
    if warnings.is_empty() {
        println!("\nnothing in the way.");
    } else {
        println!();
        for w in &warnings {
            println!("! {w}");
        }
    }
    Ok(())
}

fn converge(ctx: &Ctx, host: &Host, write: bool, only: Option<&str>) -> Result<(), String> {
    if !ctx.facts.is_pi() {
        eprintln!("! {} does not look like a Raspberry Pi", ctx.facts.get("model"));
    }
    let steps: Vec<Step> = plan::build(ctx)
        .into_iter()
        .filter(|s| only.is_none_or(|o| s.name().contains(o)))
        .collect();
    if steps.is_empty() {
        return Err(format!("no step matches `{}`", only.unwrap_or("")));
    }

    let mut changed = 0;
    let mut blocked = Vec::new();
    for step in &steps {
        let state = step.check(host);
        match (&state, write) {
            (State::Done(detail), _) => println!("  ok      {:<18} {detail}", step.name()),
            (State::Blocked(why), _) => {
                println!("  blocked {:<18} {why}", step.name());
                blocked.push(format!("{}: {}", step.name(), blocked_hint(step, why)));
            }
            (State::Todo(why), false) => {
                println!("  todo    {:<18} {why}  ({})", step.name(), step.why())
            }
            (State::Todo(_), true) => {
                print!("  apply   {:<18} ", step.name());
                use std::io::Write;
                let _ = std::io::stdout().flush();
                match step.apply(host) {
                    Err(e) => {
                        println!("FAILED");
                        return Err(format!("{} failed:\n{e}", step.name()));
                    }
                    Ok(_) => match step.check(host) {
                        State::Done(detail) => {
                            println!("{detail}");
                            changed += 1;
                        }
                        other => {
                            println!("did not take");
                            return Err(format!(
                                "{} applied but still reports `{}`: {}",
                                step.name(),
                                other.mark(),
                                other.detail()
                            ));
                        }
                    },
                }
            }
        }
    }

    println!();
    if write {
        println!("{changed} changed, {} steps total", steps.len());
    } else {
        println!("{} steps checked — run `baserri apply` to converge", steps.len());
    }
    for b in &blocked {
        println!("needs you: {b}");
    }
    Ok(())
}

fn blocked_hint(step: &Step, why: &str) -> String {
    match step {
        Step::Manual { instruction, .. } => instruction.clone(),
        _ => why.to_string(),
    }
}

fn target_dir() -> Result<PathBuf, String> {
    let out = run::cmd("cargo", &["metadata", "--format-version", "1", "--no-deps"])
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.ok() {
        return Err(format!("cargo metadata failed: {}", out.err.trim()));
    }
    let parsed = json::parse(out.out.trim()).map_err(|e| format!("cargo metadata: {e}"))?;
    parsed
        .str_at("target_directory")
        .map(PathBuf::from)
        .ok_or_else(|| "cargo metadata has no target_directory".to_string())
}

fn artifact() -> Result<PathBuf, String> {
    Ok(target_dir()?.join(TARGET).join("release").join("baserrid"))
}

fn build() -> Result<PathBuf, String> {
    if !run::which("cargo") {
        return Err("cargo is not on PATH".into());
    }
    println!("building baserrid for {TARGET} ...");
    let out = run::cmd_env(
        "cargo",
        &["build", "--release", "--target", TARGET, "--bin", "baserrid"],
        &[(LINKER_VAR, "rust-lld")],
    )
    .map_err(|e| e.to_string())?;
    if !out.ok() {
        let hint = if out.err.contains("target may not be installed") {
            format!("\n\nrun: rustup target add {TARGET}")
        } else {
            String::new()
        };
        return Err(format!("{}{hint}", out.err.trim()));
    }
    let path = artifact()?;
    if !path.exists() {
        return Err(format!("cargo reported success but {} is missing", path.display()));
    }
    Ok(path)
}

fn init(conf_path: &Path) -> Result<(), String> {
    write_if_absent(conf_path, HOST_TEMPLATE)?;
    write_if_absent(Path::new("baserrid.conf"), DAEMON_TEMPLATE)?;
    println!("\nfill in `host`, then the bot token and your chat id in baserrid.conf.");
    println!("chat id: message @userinfobot on Telegram. Both files are gitignored.");
    Ok(())
}

fn write_if_absent(path: &Path, body: &str) -> Result<(), String> {
    if path.exists() {
        println!("kept    {}", path.display());
        return Ok(());
    }
    std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote   {}", path.display());
    Ok(())
}

const HOST_TEMPLATE: &str = r#"host = baserri.local
user = pi
hostname = baserri
timezone = Europe/Madrid
lan_cidr = 192.168.1.0/24

zram_percent = 50
swap_mb = 2048
journal_max = 200M

tailscale = true
firewall = true
ssh_harden = true
unattended_upgrades = true
sudo_allowlist = true
baserrid = true

# there is no container runtime on this box by default.
# set docker = true only if you decide the eval sandbox needs one.
docker = false

forge = true
forge_url = https://baserri.your-tailnet.ts.net/
forge_port = 3000

postgres = true
pg_db = dev
pg_user = dev
pg_password =

valkey = true
valkey_password =

mailpit = true
garage = true
"#;

const DAEMON_TEMPLATE: &str = r#"token =
allow =
allow_shell = true
confirm_ttl_s = 90
timeout_s = 60
poll_timeout_s = 25

[alerts]
disk_pct = 85
temp_c = 75
interval_s = 600
units = docker, baserrid

[jobs]
uptime = uptime
containers = docker ps --format '{{.Names}}\t{{.Status}}'
"#;
