//! The little Sieve we write: one `if` block per rule, appended to the script, with the
//! `require` line kept in step. The script itself stays plain text the user can edit.

use crate::model::Mailbox;
use regex::Regex;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    From,
    To,
    Subject,
    Header,
    Body,
}

impl Field {
    pub const ALL: [Field; 5] = [Field::From, Field::To, Field::Subject, Field::Header, Field::Body];
    pub fn label(&self) -> &'static str {
        match self {
            Field::From => "from",
            Field::To => "to / cc",
            Field::Subject => "subject",
            Field::Header => "header",
            Field::Body => "body",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    Contains,
    Is,
    Matches,
    Regex,
}

impl Match {
    pub const ALL: [Match; 4] = [Match::Contains, Match::Is, Match::Matches, Match::Regex];
    pub fn label(&self) -> &'static str {
        match self {
            Match::Contains => "contains",
            Match::Is => "is",
            Match::Matches => "matches (* ?)",
            Match::Regex => "regex",
        }
    }
    fn tag(&self) -> &'static str {
        match self {
            Match::Contains => ":contains",
            Match::Is => ":is",
            Match::Matches => ":matches",
            Match::Regex => ":regex",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Move,
    Copy,
    Read,
    Flag,
    Discard,
    Redirect,
}

impl Action {
    pub const ALL: [Action; 6] = [Action::Move, Action::Copy, Action::Read, Action::Flag, Action::Discard, Action::Redirect];
    pub fn label(&self) -> &'static str {
        match self {
            Action::Move => "move to folder",
            Action::Copy => "copy to folder",
            Action::Read => "mark read",
            Action::Flag => "flag",
            Action::Discard => "discard",
            Action::Redirect => "redirect to",
        }
    }
    pub fn needs_target(&self) -> bool {
        matches!(self, Action::Move | Action::Copy | Action::Redirect)
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub field: Field,
    pub header: String,
    pub matcher: Match,
    pub value: String,
    pub action: Action,
    /// The folder for move/copy, the address for redirect.
    pub target: String,
    pub also_read: bool,
    /// Stop after this rule (no later rule sees the message).
    pub stop: bool,
}

pub fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn requires(r: &Rule) -> BTreeSet<String> {
    let mut s = BTreeSet::new();
    if matches!(r.action, Action::Move | Action::Copy) {
        s.insert("fileinto".to_string());
    }
    if r.action == Action::Copy {
        s.insert("copy".to_string());
    }
    if matches!(r.action, Action::Read | Action::Flag) || r.also_read {
        s.insert("imap4flags".to_string());
    }
    if r.matcher == Match::Regex {
        s.insert("regex".to_string());
    }
    if r.field == Field::Body {
        s.insert("body".to_string());
    }
    s
}

pub fn test(r: &Rule) -> String {
    let t = r.matcher.tag();
    let v = quote(&r.value);
    match r.field {
        Field::From => format!("address {t} \"from\" {v}"),
        Field::To => format!("address {t} [\"to\", \"cc\"] {v}"),
        Field::Subject => format!("header {t} \"subject\" {v}"),
        Field::Header => format!("header {t} {} {v}", quote(r.header.trim())),
        Field::Body => format!("body :text {t} {v}"),
    }
}

pub fn actions(r: &Rule) -> Vec<String> {
    let mut a = Vec::new();
    if r.also_read && r.action != Action::Read {
        a.push("addflag \"\\\\Seen\";".to_string());
    }
    match r.action {
        Action::Move => a.push(format!("fileinto {};", quote(&r.target))),
        Action::Copy => a.push(format!("fileinto :copy {};", quote(&r.target))),
        Action::Read => a.push("addflag \"\\\\Seen\";".to_string()),
        Action::Flag => a.push("addflag \"\\\\Flagged\";".to_string()),
        Action::Discard => a.push("discard;".to_string()),
        Action::Redirect => a.push(format!("redirect {};", quote(&r.target))),
    }
    if r.stop {
        a.push("stop;".to_string());
    }
    a
}

/// The rule as a commented `if` block.
pub fn block(r: &Rule) -> String {
    let what = format!("{}{}", r.field.label(), if r.field == Field::Header { format!(" {}", r.header.trim()) } else { String::new() });
    let does = format!("{}{}", r.action.label(), if r.target.is_empty() { String::new() } else { format!(" {}", r.target) });
    let mut s = format!("# {what} {} {} -> {does}\nif {} {{\n", r.matcher.label(), quote(&r.value), test(r));
    for a in actions(r) {
        s.push_str("    ");
        s.push_str(&a);
        s.push('\n');
    }
    s.push_str("}\n");
    s
}

fn require_re() -> Regex {
    Regex::new(r#"(?m)^[ \t]*require[ \t]+(\[[^\]]*\]|"[^"]*")[ \t]*;[ \t]*\r?\n?"#).unwrap()
}

/// The extensions the script's `require` lines ask for.
pub fn required(script: &str) -> BTreeSet<String> {
    let q = Regex::new(r#""([^"]*)""#).unwrap();
    require_re().captures_iter(script).flat_map(|c| q.captures_iter(&c[1]).map(|x| x[1].to_string()).collect::<Vec<_>>()).collect()
}

/// [script] with the rule appended and one `require` line on top covering everything.
pub fn add_rule(script: &str, r: &Rule) -> String {
    let mut have = required(script);
    have.extend(requires(r));
    let body = require_re().replace_all(script, "").trim_start_matches(['\r', '\n']).trim_end().to_string();
    let req = if have.is_empty() { String::new() } else { format!("require [{}];\n\n", have.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(", ")) };
    format!("{req}{}{}", if body.is_empty() { String::new() } else { format!("{body}\n\n") }, block(r))
}

/// The folder as `fileinto` wants it: the whole path from the top (`Inbox/lists`), not the
/// name alone, which the server takes for a top-level folder and, finding none, keeps the
/// message in the inbox.
pub fn folder_path(list: &[Mailbox], m: &Mailbox) -> String {
    let mut parts = vec![m.name.as_str()];
    let mut cur = m;
    while let Some(p) = cur.parent.as_deref().and_then(|p| list.iter().find(|x| x.id == p)) {
        if parts.len() > list.len() {
            break;
        }
        parts.push(&p.name);
        cur = p;
    }
    parts.reverse();
    parts.join("/")
}

pub const TEMPLATE: &str = "# Sieve rules, run by the server when mail arrives.\n# Add rules with the builder or write them here.\n";

/// `line 3, column 5` style positions in a server error, if it says.
pub fn error_line(msg: &str) -> Option<(u32, u32)> {
    let re = Regex::new(r"(?i)line\s*(\d+)(?:[,\s]*(?:col(?:umn)?)\s*(\d+))?").unwrap();
    let c = re.captures(msg)?;
    Some((c[1].parse().ok()?, c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> Rule {
        Rule { field: Field::From, header: String::new(), matcher: Match::Contains, value: "shop@".into(), action: Action::Move, target: "Shops".into(), also_read: true, stop: true }
    }

    #[test]
    fn appends_and_merges_require() {
        let s = add_rule("require \"vacation\";\n\n# mine\n", &rule());
        assert!(s.starts_with("require [\"fileinto\", \"imap4flags\", \"vacation\"];\n\n# mine\n\n# from contains"));
        assert!(s.contains("if address :contains \"from\" \"shop@\" {\n    addflag \"\\\\Seen\";\n    fileinto \"Shops\";\n    stop;\n}\n"));
        let s2 = add_rule(&s, &Rule { action: Action::Flag, also_read: false, ..rule() });
        assert_eq!(s2.matches("require").count(), 1);
    }

    #[test]
    fn folder_paths() {
        let mb = |id: &str, name: &str, parent: Option<&str>| Mailbox { account: 1, id: id.into(), name: name.into(), role: None, parent: parent.map(Into::into), sort: 0, total: 0, unread: 0, notify: None };
        let list = vec![mb("a", "Inbox", None), mb("b", "lists", Some("a")), mb("c", "2026", Some("b")), mb("d", "Old", None), mb("e", "lost", Some("zz"))];
        assert_eq!(folder_path(&list, &list[0]), "Inbox");
        assert_eq!(folder_path(&list, &list[1]), "Inbox/lists");
        assert_eq!(folder_path(&list, &list[2]), "Inbox/lists/2026");
        assert_eq!(folder_path(&list, &list[3]), "Old");
        assert_eq!(folder_path(&list, &list[4]), "lost");
    }

    #[test]
    fn error_positions() {
        assert_eq!(error_line("Invalid script at line 4, column 7: oops"), Some((4, 7)));
        assert_eq!(error_line("nothing"), None);
    }
}
