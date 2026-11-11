fn main() {
    // Cargo sets TARGET for build scripts to the triple the crate (and the test binaries
    // linking it) is compiled for, which the test layout parsing compares path segments
    // against.
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=FONTAINE_TESTKIT_TARGET={target}");
    // The rustc cargo invokes for the outer build; the nested build run from the test
    // process uses the same toolchain, so its `--print target-list` is the authority on
    // which `--target` names resolve without a custom spec file.
    let rustc = std::env::var("RUSTC").expect("cargo sets RUSTC for build scripts");
    println!("cargo:rustc-env=FONTAINE_TESTKIT_RUSTC={rustc}");
}
