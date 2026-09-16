use crate::json::{self, Value};
use crate::run;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

pub struct Request<'a> {
    pub url: &'a str,
    pub headers: Vec<String>,
    pub body: Option<&'a str>,
    pub max_time: u64,
}

impl<'a> Request<'a> {
    pub fn get(url: &'a str) -> Request<'a> {
        Request { url, headers: Vec::new(), body: None, max_time: 30 }
    }

    pub fn post(url: &'a str, body: &'a str) -> Request<'a> {
        Request {
            url,
            headers: vec!["Content-Type: application/json".into()],
            body: Some(body),
            max_time: 30,
        }
    }

    pub fn header(mut self, h: impl Into<String>) -> Request<'a> {
        self.headers.push(h.into());
        self
    }

    pub fn timeout(mut self, seconds: u64) -> Request<'a> {
        self.max_time = seconds;
        self
    }
}

pub fn config_for(req: &Request, body_path: Option<&str>) -> String {
    let mut c = format!("url = \"{}\"\n", req.url);
    for h in &req.headers {
        c.push_str(&format!("header = \"{}\"\n", h.replace('\\', "\\\\").replace('"', "\\\"")));
    }
    if let Some(p) = body_path {
        c.push_str(&format!("data-binary = \"@{p}\"\n"));
    }
    c.push_str(&format!("silent\nshow-error\nlocation\nmax-time = \"{}\"\n", req.max_time));
    c
}

pub fn send(req: &Request) -> Result<String, String> {
    let staged = req.body.map(|body| {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("baserri-req-{}-{seq}", std::process::id()));
        (path, body)
    });
    let body_path = match &staged {
        Some((path, body)) => {
            fs::write(path, body).map_err(|e| format!("staging the request body: {e}"))?;
            Some(path.display().to_string())
        }
        None => None,
    };
    let config = config_for(req, body_path.as_deref());
    let out = run::cmd_stdin("curl", &["-K", "-"], Some(&config));
    if let Some((path, _)) = &staged {
        let _ = fs::remove_file(path);
    }
    let out = out.map_err(|e| format!("curl: {e}"))?;
    if !out.ok() {
        return Err(format!("curl exited {}: {}", out.code, out.err.trim()));
    }
    Ok(out.out)
}

pub fn send_json(req: &Request) -> Result<Value, String> {
    let body = send(req)?;
    json::parse(body.trim()).map_err(|e| format!("bad json response: {e}"))
}

pub fn temp_dir_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_reaches_argv() {
        let req = Request::get("https://api.example.com/bot123:SECRET/x");
        let config = config_for(&req, None);
        assert!(config.contains("url = \"https://api.example.com/bot123:SECRET/x\""));
        assert!(config.contains("silent"));
    }

    #[test]
    fn headers_with_quotes_are_escaped_for_the_curl_config() {
        let req = Request::get("https://x").header("X-Odd: a\"b");
        assert!(config_for(&req, None).contains("header = \"X-Odd: a\\\"b\""));
    }

    #[test]
    fn a_body_is_referenced_by_file_not_inlined() {
        let req = Request::post("https://x", "{\"a\":1}");
        let config = config_for(&req, Some("/tmp/body"));
        assert!(config.contains("data-binary = \"@/tmp/body\""));
        assert!(!config.contains("{\"a\":1}"));
    }

    #[test]
    fn timeout_is_carried_through() {
        assert!(config_for(&Request::get("https://x").timeout(90), None).contains("max-time = \"90\""));
    }

    #[test]
    fn it_really_talks_to_curl() {
        let path = temp_dir_path(&format!("baserri-http-test-{}", std::process::id()));
        fs::write(&path, "hello from a file").unwrap();
        let url = format!("file://{}", path.display());
        let out = send(&Request::get(&url));
        fs::remove_file(&path).ok();
        assert_eq!(out.unwrap().trim(), "hello from a file");
    }

    #[test]
    fn a_failing_transfer_is_an_error_not_an_empty_body() {
        let url = format!("file://{}", temp_dir_path("baserri-does-not-exist").display());
        assert!(send(&Request::get(&url)).is_err());
    }
}
