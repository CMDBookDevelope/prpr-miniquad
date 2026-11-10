use std::env;

fn main() {
    let target = env::var("TARGET").unwrap_or_else(|e| panic!("{}", e));

    if target.contains("darwin") {
        println!("cargo:rustc-link-lib=framework=OpenGL");
    }
    if (target.contains("darwin") || target.contains("ios")) && env::var_os("CARGO_FEATURE_METAL").is_some() {
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }

    if target.contains("ios") {
        println!("cargo:rustc-link-lib=framework=OpenGLES");
    }
    //ohos uses napi_build_ohos to make a napi module
    #[cfg(target_env = "ohos")]
    napi_build_ohos::setup();
}
