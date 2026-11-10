fn main() {
    // Compiled exactly as in `firmware/build.rs`.
    slint_build::compile_with_config(
        "../../firmware/ui/wakey.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent-dark".into())
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .expect("Slint build failed");
}
