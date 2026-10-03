//! maildot core: everything but the window. Mail over JMAP (sync, push, local store,
//! PGP, sending, Sieve), parallel local search, calendars over CalDAV, shelf.

pub mod cal;
pub mod caldav;
pub mod db;
pub mod engine;
pub mod html;
pub mod ical;
pub mod jmap;
pub mod mime;
pub mod model;
pub mod paths;
pub mod pgp;
pub mod search;
pub mod secrets;
pub mod senders;
pub mod shelf;
pub mod sieve;
pub mod sync;

pub use chrono_tz;
pub use sequoia_openpgp;
