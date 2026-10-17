fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // app.rc embeds our own manifest; stop the MSVC linker from adding a second, default one
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        }
        embed_resource::compile_for("app.rc", ["monarch-next"], embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
