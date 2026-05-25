#[expect(
    clippy::expect_used,
    reason = "build scripts panic on failure by convention"
)]
fn main() {
    slint_build::compile("src/ui/main.slint").expect("slint compile failed");
}
