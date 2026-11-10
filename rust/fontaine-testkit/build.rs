fn main() {
    // Cargo sets TARGET for build scripts to the triple the crate (and the test binaries
    // linking it) is compiled for, which the test layout parsing compares path segments
    // against.
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=FONTAINE_TESTKIT_TARGET={target}");
}
