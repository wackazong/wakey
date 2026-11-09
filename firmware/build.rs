fn main() {
    // One Slint file per binary, so each embeds only its own pre-rendered
    // glyphs (a 300 px digit set is megabytes). The generated code lands in
    // `$OUT_DIR/<stem>.rs`; binaries `include!` their own instead of
    // `slint::include_modules!()`, which only knows the last compiled file.
    for ui in ["ui/slint_test.slint", "ui/wakey.slint"] {
        slint_build::compile_with_config(
            ui,
            slint_build::CompilerConfiguration::new()
                .with_style("fluent-dark".into())
                .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
        )
        .expect("Slint build failed");
    }

    // Fallback for the clock when the RTC has no valid time.
    let build_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_UNIX_TIME={build_time}");

    embuild::espidf::sysenv::output();
}
