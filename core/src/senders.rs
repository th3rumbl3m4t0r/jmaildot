//! Which addresses are ours to write from. The server only tells us its identities; the
//! catch-all aliases are inferred: an address that received mail as the only recipient is
//! one of ours, and so is its domain. Anything else typed in `from` is allowed, with a warning.

use crate::db::Db;
use crate::model::{Account, Addr};
use std::collections::BTreeSet;

/// Identity domains, the account's sending domains, and every domain received mail was addressed to.
pub fn domains(db: &Db, a: &Account) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = a.identities.iter().filter_map(|i| i.email.rsplit_once('@').map(|(_, d)| d.to_lowercase())).collect();
    if let Some((_, d)) = a.email.rsplit_once('@') {
        out.insert(d.to_lowercase());
    }
    out.extend(a.sender_domains.iter().cloned());
    for (addr, _) in db.seen_rcpts(a.id) {
        if let Some((_, d)) = addr.rsplit_once('@') {
            out.insert(d.to_lowercase());
        }
    }
    out
}

/// Identity addresses first, then received-to addresses whose domain is ours.
pub fn addresses(db: &Db, a: &Account) -> Vec<String> {
    let ds = domains(db, a);
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s)
        }
    };
    push(a.email.to_lowercase());
    for i in &a.identities {
        push(i.email.to_lowercase());
    }
    for (addr, _) in db.seen_rcpts(a.id) {
        if addr.rsplit_once('@').map(|(_, d)| ds.contains(d)).unwrap_or(false) {
            push(addr);
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

pub fn known(db: &Db, a: &Account, email: &str) -> bool {
    email.rsplit_once('@').map(|(_, d)| domains(db, a).contains(&d.to_lowercase())).unwrap_or(false)
}

/// The display name to put on [alias]: the identity's name when it is a real name; when it
/// is just an address (Stalwart's default) the alias itself, so an alias never carries the
/// primary address in front of it.
pub fn display_name(identity_name: Option<&str>, primary: &str, alias: &str) -> String {
    let n = identity_name.unwrap_or("").trim();
    if n.is_empty() || n.contains('@') || n.eq_ignore_ascii_case(primary) {
        alias.to_string()
    } else {
        n.to_string()
    }
}

pub fn with_name(a: &Account, email: &str) -> Addr {
    let name = a.display_name.clone().or_else(|| a.identities.iter().find(|i| i.email.eq_ignore_ascii_case(&a.email)).and_then(|i| i.name.clone()));
    Addr::new(Some(&display_name(name.as_deref(), &a.email, email)), email)
}

pub fn default_from(a: &Account) -> Addr {
    with_name(a, &a.email)
}

/// The address to answer from: the first To / Cc address that is ours (by address, then by domain), else the default.
pub fn reply_from(db: &Db, a: &Account, to: &[Addr], cc: &[Addr]) -> Addr {
    let mine = addresses(db, a);
    let rcpts: Vec<&Addr> = to.iter().chain(cc.iter()).collect();
    if let Some(x) = rcpts.iter().find(|r| mine.contains(&r.email.to_lowercase())) {
        return with_name(a, &x.email);
    }
    if let Some(x) = rcpts.iter().find(|r| known(db, a, &r.email)) {
        return with_name(a, &x.email);
    }
    default_from(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(display_name(Some("alice@x.cz"), "alice@x.cz", "shop@x.cz"), "shop@x.cz");
        assert_eq!(display_name(Some("Alice Novak"), "alice@x.cz", "shop@x.cz"), "Alice Novak");
        assert_eq!(display_name(None, "alice@x.cz", "shop@x.cz"), "shop@x.cz");
    }
}
