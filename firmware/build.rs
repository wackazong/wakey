fn main() {
    // One Slint file per program, so each embeds only its own pre-rendered
    // glyphs (Slint renders every imported font face at every pixel size a
    // file uses; the clock's 280 px digits are megabytes). The generated
    // code lands in `$OUT_DIR/<stem>.rs` and is `include!`d by its user
    // (`slint::include_modules!()` only knows the last compiled file).
    for ui in ["ui/wakey.slint", "ui/ui_bench.slint"] {
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
