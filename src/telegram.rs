use crate::json::{self, Value};
use crate::run;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub id: i64,
    pub chat: i64,
    pub from: i64,
    pub name: String,
    pub text: String,
}

#[derive(Clone)]
pub struct Api {
    token: String,
    tmp: PathBuf,
}

impl Api {
    pub fn new(token: &str) -> Api {
        Api { token: token.to_string(), tmp: std::env::temp_dir() }
    }

    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    fn call(&self, method: &str, body: &str, max_time: u64) -> Result<Value, String> {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let body_path = self.tmp.join(format!("baserrid-{}-{seq}.json", std::process::id()));
        fs::write(&body_path, body).map_err(|e| format!("staging the request body: {e}"))?;
        let config = format!(
            "url = \"{}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\nsilent\nshow-error\nmax-time = \"{max_time}\"\n",
            self.url(method),
            body_path.display()
        );
        let out = run::cmd_stdin("curl", &["-K", "-"], Some(&config));
        let _ = fs::remove_file(&body_path);
        let out = out.map_err(|e| format!("curl: {e}"))?;
        if !out.ok() {
            return Err(format!("curl exited {}: {}", out.code, out.err.trim()));
        }
        let parsed = json::parse(out.out.trim()).map_err(|e| format!("bad json from telegram: {e}"))?;
        if parsed.path("ok").and_then(Value::as_bool) != Some(true) {
            let why = parsed.str_at("description").unwrap_or("telegram rejected the call");
            return Err(why.to_string());
        }
        Ok(parsed)
    }

    pub fn get_updates(&self, offset: i64, timeout: u64) -> Result<Vec<Update>, String> {
        let body = format!(
            "{{\"offset\":{offset},\"timeout\":{timeout},\"allowed_updates\":[\"message\"]}}"
        );
        let v = self.call("getUpdates", &body, timeout + 15)?;
        Ok(parse_updates(&v))
    }

    pub fn send(&self, chat: i64, text: &str) -> Result<(), String> {
        let body = format!(
            "{{\"chat_id\":{chat},\"parse_mode\":\"HTML\",\"disable_web_page_preview\":true,\"text\":{}}}",
            json::escape(text)
        );
        self.call("sendMessage", &body, 30).map(|_| ())
    }

    pub fn send_block(&self, chat: i64, title: &str, body: &str) -> Result<(), String> {
        self.send(chat, &format_block(title, body))
    }
}

pub const MAX_MESSAGE: usize = 3500;

pub fn format_block(title: &str, body: &str) -> String {
    let trimmed = body.trim_end();
    let shown = clip(trimmed, MAX_MESSAGE);
    if shown.is_empty() {
        format!("<b>{}</b>\n<i>no output</i>", escape_html(title))
    } else {
        format!("<b>{}</b>\n<pre>{}</pre>", escape_html(title), escape_html(&shown))
    }
}

pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... [{} bytes truncated]", &text[..end], text.len() - end)
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn parse_updates(v: &Value) -> Vec<Update> {
    let Some(items) = v.path("result").and_then(Value::as_arr) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|u| {
            let id = u.i64_at("update_id")?;
            let msg = u.path("message")?;
            Some(Update {
                id,
                chat: msg.i64_at("chat.id")?,
                from: msg.i64_at("from.id").unwrap_or(0),
                name: msg
                    .str_at("from.username")
                    .or_else(|| msg.str_at("from.first_name"))
                    .unwrap_or("unknown")
                    .to_string(),
                text: msg.str_at("text").unwrap_or("").to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_without_a_text_message_are_skipped_not_fatal() {
        let v = json::parse(
            r#"{"ok":true,"result":[
                {"update_id":1,"message":{"chat":{"id":7},"from":{"id":7,"username":"eneko"},"text":"/status"}},
                {"update_id":2,"edited_message":{"chat":{"id":7}}},
                {"update_id":3,"message":{"chat":{"id":7},"from":{"id":7},"photo":[]}}
            ]}"#,
        )
        .unwrap();
        let ups = parse_updates(&v);
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0].text, "/status");
        assert_eq!(ups[0].name, "eneko");
        assert_eq!(ups[1].text, "");
        assert_eq!(ups[1].name, "unknown");
    }

    #[test]
    fn an_empty_result_is_an_empty_list() {
        assert!(parse_updates(&json::parse(r#"{"ok":true,"result":[]}"#).unwrap()).is_empty());
        assert!(parse_updates(&json::parse(r#"{"ok":true}"#).unwrap()).is_empty());
    }

    #[test]
    fn html_is_escaped_so_output_cannot_break_the_message() {
        let block = format_block("ls", "<script>a & b</script>");
        assert!(block.contains("&lt;script&gt;a &amp; b&lt;/script&gt;"));
        assert!(block.starts_with("<b>ls</b>"));
    }

    #[test]
    fn clipping_never_splits_a_character() {
        let text = "ñ".repeat(4000);
        let out = clip(&text, MAX_MESSAGE);
        assert!(out.contains("bytes truncated"));
        assert!(out.is_char_boundary(0));
        let _ = out.chars().count();
    }

    #[test]
    fn short_output_is_not_clipped() {
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn empty_output_says_so() {
        assert!(format_block("df", "   \n").contains("no output"));
    }
}
