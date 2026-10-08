fn main() {
    // Native GPUI composition in unoptimized builds exceeds the MSVC 1 MiB
    // main-thread default. Reserve virtual space; committed memory grows on demand.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rerun-if-changed=app.rc");
        println!("cargo:rerun-if-changed=../../assets/branding/opsssh.ico");
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_required()
            .expect("compile the OpsSSH Windows icon resource");
        println!("cargo:rustc-link-arg-bin=opsssh=/STACK:8388608");
    }
}
