fn main() {
    // Native GPUI composition in unoptimized builds exceeds the MSVC 1 MiB
    // main-thread default. Reserve virtual space; committed memory grows on demand.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=opsssh=/STACK:8388608");
    }
}
