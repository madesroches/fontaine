//! The scripted language server of `fontaine-tools`, built in this package so the headless
//! tests can start it.

#[path = "../../fontaine-tools/examples/fake_lsp.rs"]
mod fake_lsp;

fn main() {
    fake_lsp::main();
}
