pub mod ask_user;
pub mod builtins;
pub mod child_env;
pub mod descriptor;
pub mod error_chain;
pub mod gate;
pub mod git_bash;
pub mod guarded;
pub mod lock;
pub mod names;
pub mod preclassify;
pub mod result_pipeline;
pub mod risk;
pub mod schedule;
pub mod scope;
pub mod skill;
pub mod tls;
pub mod todo;
pub mod webfetch;
pub mod websearch;

// Wikimedia and many CDNs answer a request without a User-Agent with 403; gateways that ask
// clients to identify themselves read it too.
pub const FONTAINE_USER_AGENT: &str = concat!(
    "fontaine/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/madesroches/fontaine)"
);
