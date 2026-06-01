//! Build script: compiles `src/slint/main.slint` into Rust via `slint-build`.

#[expect(
    clippy::expect_used,
    reason = "build scripts panic on failure by convention"
)]
fn main() {
    slint_build::compile("src/slint/main.slint").expect("slint compile failed");
}
