fn main() {
    // UI test screen for the `slint_test` binary. Fonts and images are
    // pre-rendered at build time for the software renderer.
    slint_build::compile_with_config(
        "ui/slint_test.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent-dark".into())
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .expect("Slint build failed");

    embuild::espidf::sysenv::output();
}
