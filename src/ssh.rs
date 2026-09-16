use crate::conf::Conf;
use crate::run::{self, Out};
use std::io;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Host {
    pub user: String,
    pub addr: String,
    pub port: u16,
    pub control: String,
    pub identity: Option<String>,
}

const PREAMBLE: &str = "set -euo pipefail\nexport LC_ALL=C DEBIAN_FRONTEND=noninteractive\n";

impl Host {
    pub fn from_conf(c: &Conf) -> Result<Host, String> {
        let addr = c.req("host")?.to_string();
        Ok(Host {
            user: c.or("user", "pi").to_string(),
            addr,
            port: c.num("port", 22) as u16,
            control: c.or("control_path", "/tmp/baserri-ssh-%C").to_string(),
            identity: c.get("identity").map(str::to_string),
        })
    }

    pub fn target(&self) -> String {
        format!("{}@{}", self.user, self.addr)
    }

    fn opts(&self) -> Vec<String> {
        let mut v = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", self.control),
            "-o".into(),
            "ControlPersist=120".into(),
        ];
        if let Some(id) = &self.identity {
            v.push("-i".into());
            v.push(id.clone());
        }
        v
    }

    pub fn sh(&self, script: &str) -> io::Result<Out> {
        self.exec(script, false)
    }

    pub fn sudo(&self, script: &str) -> io::Result<Out> {
        self.exec(script, true)
    }

    fn exec(&self, script: &str, root: bool) -> io::Result<Out> {
        let mut args: Vec<String> = self.opts();
        args.push("-p".into());
        args.push(self.port.to_string());
        args.push(self.target());
        if root {
            args.push("sudo".into());
            args.push("-n".into());
        }
        args.push("bash".into());
        args.push("-s".into());
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let body = format!("{PREAMBLE}{script}\n");
        run::cmd_stdin("ssh", &borrowed, Some(&body))
    }

    pub fn put(&self, local: &Path, remote: &str) -> io::Result<Out> {
        let mut args: Vec<String> = self.opts();
        args.push("-P".into());
        args.push(self.port.to_string());
        args.push(local.display().to_string());
        args.push(format!("{}:{remote}", self.target()));
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        run::cmd("scp", &borrowed)
    }

    pub fn reachable(&self) -> Result<(), String> {
        match self.sh("echo up") {
            Ok(o) if o.ok() => Ok(()),
            Ok(o) => Err(format!("ssh to {} failed: {}", self.target(), o.last_line())),
            Err(e) => Err(format!("could not run ssh: {e}")),
        }
    }

    pub fn can_sudo(&self) -> bool {
        self.sudo("true").map(|o| o.ok()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = Conf::parse("host = baserri.local\n").unwrap();
        let h = Host::from_conf(&c).unwrap();
        assert_eq!(h.target(), "pi@baserri.local");
        assert_eq!(h.port, 22);

        let c = Conf::parse("host = 10.0.0.5\nuser = eneko\nport = 2222\n").unwrap();
        let h = Host::from_conf(&c).unwrap();
        assert_eq!(h.target(), "eneko@10.0.0.5");
        assert_eq!(h.port, 2222);
    }

    #[test]
    fn a_missing_host_is_an_error_not_a_default() {
        assert!(Host::from_conf(&Conf::parse("user = pi\n").unwrap()).is_err());
    }

    #[test]
    fn opts_carry_multiplexing_and_batch_mode() {
        let h = Host::from_conf(&Conf::parse("host = x\n").unwrap()).unwrap();
        let joined = h.opts().join(" ");
        assert!(joined.contains("BatchMode=yes"));
        assert!(joined.contains("ControlMaster=auto"));
        assert!(joined.contains("ControlPath=/tmp/baserri-ssh-%C"));
    }
}
