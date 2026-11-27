fn main() {
    println!("cargo:rerun-if-changed=src/gpu_probe.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("src/gpu_probe.m").flag("-fobjc-arc").flag("-fblocks").compile("bench_gpu_probe");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
