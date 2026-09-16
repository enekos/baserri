use crate::http::{self, Request};
use crate::json::{self, Value};
use crate::watch::Change;

pub const API: &str = "https://api.github.com/graphql";
pub const REST: &str = "https://api.github.com";

pub const QUERY: &str = "query { viewer { login pullRequests(states: OPEN, first: 50, orderBy: {field: UPDATED_AT, direction: DESC}) { nodes { number title isDraft mergeable reviewDecision repository { nameWithOwner } commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } } } } }";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub draft: bool,
    pub checks: String,
    pub review: String,
    pub mergeable: String,
    pub title: String,
}

impl Snapshot {
    pub fn render(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            if self.draft { "draft" } else { "ready" },
            self.checks,
            self.review,
            self.mergeable,
            self.title
        )
    }

    pub fn parse(text: &str) -> Snapshot {
        let mut parts = text.splitn(5, '|');
        Snapshot {
            draft: parts.next() == Some("draft"),
            checks: parts.next().unwrap_or("NONE").to_string(),
            review: parts.next().unwrap_or("NONE").to_string(),
            mergeable: parts.next().unwrap_or("UNKNOWN").to_string(),
            title: parts.next().unwrap_or_default().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pr {
    pub repo: String,
    pub number: i64,
    pub snapshot: Snapshot,
}

impl Pr {
    pub fn key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }
}

pub fn url_for(key: &str) -> String {
    match key.split_once('#') {
        Some((repo, number)) => format!("https://github.com/{repo}/pull/{number}"),
        None => String::new(),
    }
}

pub fn parse_prs(v: &Value) -> Vec<Pr> {
    let Some(nodes) = v.path("data.viewer.pullRequests.nodes").and_then(Value::as_arr) else {
        return Vec::new();
    };
    nodes
        .iter()
        .filter_map(|n| {
            let repo = n.str_at("repository.nameWithOwner")?.to_string();
            let number = n.i64_at("number")?;
            Some(Pr {
                repo,
                number,
                snapshot: Snapshot {
                    draft: n.path("isDraft").and_then(Value::as_bool).unwrap_or(false),
                    checks: n
                        .str_at("commits.nodes.0.commit.statusCheckRollup.state")
                        .unwrap_or("NONE")
                        .to_string(),
                    review: n.str_at("reviewDecision").unwrap_or("NONE").to_string(),
                    mergeable: n.str_at("mergeable").unwrap_or("UNKNOWN").to_string(),
                    title: n.str_at("title").unwrap_or("").to_string(),
                },
            })
        })
        .collect()
}

pub fn describe(change: &Change) -> Option<String> {
    let key = change.key().to_string();
    let link = format!("<a href=\"{}\">{}</a>", url_for(&key), crate::telegram::escape_html(&key));
    match change {
        Change::Appeared { now, .. } => {
            let s = Snapshot::parse(now);
            Some(format!(
                "\u{1f195} {link} opened{}\n{}",
                if s.draft { " as a draft" } else { "" },
                crate::telegram::escape_html(&s.title)
            ))
        }
        Change::Gone { .. } => None,
        Change::Changed { was, now, .. } => {
            let (a, b) = (Snapshot::parse(was), Snapshot::parse(now));
            let mut lines = Vec::new();
            if a.draft && !b.draft {
                lines.push("marked ready for review".to_string());
            }
            if !a.draft && b.draft {
                lines.push("moved back to draft".to_string());
            }
            if a.checks != b.checks
                && let Some(text) = checks_line(&a.checks, &b.checks)
            {
                lines.push(text);
            }
            if a.review != b.review
                && let Some(text) = review_line(&b.review)
            {
                lines.push(text);
            }
            if a.mergeable != b.mergeable
                && let Some(text) = mergeable_line(&a.mergeable, &b.mergeable)
            {
                lines.push(text);
            }
            if lines.is_empty() {
                return None;
            }
            Some(format!(
                "{link}\n{}\n<i>{}</i>",
                lines.join("\n"),
                crate::telegram::escape_html(&b.title)
            ))
        }
    }
}

fn checks_line(was: &str, now: &str) -> Option<String> {
    match now {
        "FAILURE" | "ERROR" => Some("\u{274c} CI went red".into()),
        "SUCCESS" if was == "FAILURE" || was == "ERROR" => Some("\u{2705} CI is green again".into()),
        "SUCCESS" => Some("\u{2705} CI passed".into()),
        _ => None,
    }
}

fn review_line(now: &str) -> Option<String> {
    match now {
        "APPROVED" => Some("\u{1f44d} approved".into()),
        "CHANGES_REQUESTED" => Some("\u{1f4dd} changes requested".into()),
        _ => None,
    }
}

fn mergeable_line(was: &str, now: &str) -> Option<String> {
    match now {
        "CONFLICTING" => Some("\u{26a0} conflicts with the base branch".into()),
        "MERGEABLE" if was == "CONFLICTING" => Some("conflicts resolved".into()),
        _ => None,
    }
}

pub struct Client {
    token: String,
}

impl Client {
    pub fn new(token: &str) -> Client {
        Client { token: token.to_string() }
    }

    fn headers<'a>(&self, req: Request<'a>) -> Request<'a> {
        req.header(format!("Authorization: Bearer {}", self.token))
            .header("User-Agent: baserri")
            .header("Accept: application/vnd.github+json")
    }

    pub fn open_prs(&self) -> Result<Vec<Pr>, String> {
        let body = format!("{{\"query\":{}}}", json::escape(QUERY));
        let req = self.headers(Request::post(API, &body).timeout(45));
        let v = http::send_json(&req)?;
        if let Some(errors) = v.path("errors").and_then(Value::as_arr)
            && let Some(first) = errors.first().and_then(|e| e.str_at("message"))
        {
            return Err(format!("github: {first}"));
        }
        Ok(parse_prs(&v))
    }

    pub fn closed_how(&self, key: &str) -> String {
        let Some((repo, number)) = key.split_once('#') else {
            return "closed".into();
        };
        let url = format!("{REST}/repos/{repo}/pulls/{number}");
        let req = self.headers(Request::get(&url));
        match http::send_json(&req) {
            Ok(v) if v.path("merged").and_then(Value::as_bool) == Some(true) => "merged".into(),
            Ok(_) => "closed without merging".into(),
            Err(_) => "closed".into(),
        }
    }
}

pub fn board(prs: &[Pr]) -> String {
    if prs.is_empty() {
        return "no open pull requests".into();
    }
    let mut s = format!("<b>{} open PR(s)</b>\n", prs.len());
    for pr in prs {
        let mark = match pr.snapshot.checks.as_str() {
            "SUCCESS" => "\u{2705}",
            "FAILURE" | "ERROR" => "\u{274c}",
            "PENDING" => "\u{1f7e1}",
            _ => "\u{2b1c}",
        };
        let flags = [
            pr.snapshot.draft.then_some("draft"),
            (pr.snapshot.mergeable == "CONFLICTING").then_some("conflicts"),
            (pr.snapshot.review == "APPROVED").then_some("approved"),
            (pr.snapshot.review == "CHANGES_REQUESTED").then_some("changes requested"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        s.push_str(&format!(
            "{mark} <a href=\"{}\">{}</a> {}{}\n",
            pr.url(),
            crate::telegram::escape_html(&pr.key()),
            crate::telegram::escape_html(&pr.snapshot.title),
            if flags.is_empty() { String::new() } else { format!(" — {flags}") }
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &str = r#"{"data":{"viewer":{"login":"enekos","pullRequests":{"nodes":[
        {"number":13276,"title":"chore: let the proxy re-point redirects","isDraft":true,"mergeable":"MERGEABLE","reviewDecision":null,
         "repository":{"nameWithOwner":"join-com/frontend"},
         "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]}},
        {"number":9,"title":"odei v0.2.2","isDraft":false,"mergeable":"CONFLICTING","reviewDecision":"APPROVED",
         "repository":{"nameWithOwner":"enekos/odei"},
         "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}}
    ]}}}}"#;

    #[test]
    fn parses_a_real_shaped_graphql_reply() {
        let prs = parse_prs(&json::parse(PAYLOAD).unwrap());
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].key(), "join-com/frontend#13276");
        assert_eq!(prs[0].url(), "https://github.com/join-com/frontend/pull/13276");
        assert!(prs[0].snapshot.draft);
        assert_eq!(prs[0].snapshot.checks, "FAILURE");
        assert_eq!(prs[0].snapshot.review, "NONE");
        assert_eq!(prs[1].snapshot.checks, "NONE");
        assert_eq!(prs[1].snapshot.mergeable, "CONFLICTING");
    }

    #[test]
    fn an_empty_or_broken_reply_is_no_prs_not_a_panic() {
        assert!(parse_prs(&json::parse(r#"{"data":{"viewer":{"pullRequests":{"nodes":[]}}}}"#).unwrap()).is_empty());
        assert!(parse_prs(&json::parse(r#"{"errors":[{"message":"Bad credentials"}]}"#).unwrap()).is_empty());
    }

    #[test]
    fn a_snapshot_round_trips_through_the_transition_store() {
        let s = Snapshot {
            draft: true,
            checks: "FAILURE".into(),
            review: "CHANGES_REQUESTED".into(),
            mergeable: "CONFLICTING".into(),
            title: "a title with | a pipe in it".into(),
        };
        assert_eq!(Snapshot::parse(&s.render()), s);
    }

    #[test]
    fn ci_going_red_is_reported_and_ci_merely_starting_is_not() {
        let red = Change::Changed {
            key: "a/b#1".into(),
            was: "ready|SUCCESS|NONE|MERGEABLE|t".into(),
            now: "ready|FAILURE|NONE|MERGEABLE|t".into(),
        };
        assert!(describe(&red).unwrap().contains("CI went red"));

        let started = Change::Changed {
            key: "a/b#1".into(),
            was: "ready|SUCCESS|NONE|MERGEABLE|t".into(),
            now: "ready|PENDING|NONE|MERGEABLE|t".into(),
        };
        assert!(describe(&started).is_none(), "a run starting is noise, not news");
    }

    #[test]
    fn several_changes_in_one_poll_become_one_message() {
        let c = Change::Changed {
            key: "a/b#1".into(),
            was: "draft|PENDING|NONE|MERGEABLE|t".into(),
            now: "ready|SUCCESS|APPROVED|CONFLICTING|t".into(),
        };
        let msg = describe(&c).unwrap();
        for expected in ["marked ready for review", "CI passed", "approved", "conflicts with the base"] {
            assert!(msg.contains(expected), "missing `{expected}` in {msg}");
        }
    }

    #[test]
    fn a_change_with_nothing_worth_saying_says_nothing() {
        let c = Change::Changed {
            key: "a/b#1".into(),
            was: "ready|NONE|NONE|UNKNOWN|old title".into(),
            now: "ready|NONE|NONE|MERGEABLE|old title".into(),
        };
        assert!(describe(&c).is_none());
    }

    #[test]
    fn opening_a_pr_is_announced_with_its_draft_state() {
        let c = Change::Appeared { key: "a/b#7".into(), now: "draft|NONE|NONE|MERGEABLE|new work".into() };
        let msg = describe(&c).unwrap();
        assert!(msg.contains("opened as a draft"));
        assert!(msg.contains("new work"));
        assert!(msg.contains("https://github.com/a/b/pull/7"));
    }

    #[test]
    fn a_pr_leaving_the_list_is_handled_by_the_caller_not_here() {
        assert!(describe(&Change::Gone { key: "a/b#1".into(), was: String::new() }).is_none());
    }

    #[test]
    fn html_in_a_pr_title_cannot_break_the_message() {
        let c = Change::Appeared { key: "a/b#7".into(), now: "ready|NONE|NONE|MERGEABLE|fix <script>".into() };
        assert!(describe(&c).unwrap().contains("fix &lt;script&gt;"));
    }

    #[test]
    fn the_board_marks_each_pr_by_its_checks() {
        let prs = parse_prs(&json::parse(PAYLOAD).unwrap());
        let b = board(&prs);
        assert!(b.contains("2 open PR(s)"));
        assert!(b.contains("\u{274c}"));
        assert!(b.contains("draft"));
        assert!(b.contains("conflicts"));
        assert!(b.contains("approved"));
        assert_eq!(board(&[]), "no open pull requests");
    }
}
