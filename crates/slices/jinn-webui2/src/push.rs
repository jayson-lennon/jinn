//! Web Push: how a turn's end reaches a browser whose page is not running.
//!
//! Each instance signs as itself. The server generates a P-256 key pair at
//! first need and keeps it, beside every browser's subscription, in one file
//! under the data dir — nothing in the repository, nothing in config, and no
//! registration with any push service: every browser vendor's service accepts
//! the standard protocol with a self-generated key.
//!
//! A push is sent when a session lands on Idle — the same fact that turns its
//! row gold — and to every subscription, one request each. The page decides
//! nothing on this path: whether a banner is *shown* is the service worker's
//! call, because only it can see whether any page of the origin is visible.
//!
//! The subscription's endpoint is a bearer capability — whoever holds it can
//! send to that browser — so it is stored as a secret and never logged.

pub mod keys;
pub mod notify;
pub mod store;
pub mod webpush;
