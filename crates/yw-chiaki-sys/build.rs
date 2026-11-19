// 调用 tools/build-shim-macos.sh 编译 libchiaki 外壳并合并静态库，然后告诉 cargo 怎么链接。
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..").canonicalize().expect("找不到项目根目录");
    let script = root.join("tools/build-shim-macos.sh");

    for p in [
        "crates/yw-chiaki-sys/shim/yw_shim.c",
        "crates/yw-chiaki-sys/shim/yw_shim.h",
        "crates/yw-chiaki-sys/shim/CMakeLists.txt",
        "crates/yw-chiaki-sys/shim/linkcheck.c",
        "tools/build-shim-macos.sh",
        "tools/fetch-chiaki.sh",
        "third_party/patches",
        // 改了 chiaki 源码或重建了依赖库也要重编（目录会被递归扫描）
        "third_party/chiaki-ng/lib",
        "third_party/deps/macos-arm64/lib",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(p).display());
    }

    let out = Command::new("bash")
        .arg(&script)
        .output()
        .expect("无法运行 tools/build-shim-macos.sh");
    if !out.status.success() {
        panic!(
            "编译 libchiaki 外壳失败：\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let frameworks = stdout.lines().last().unwrap_or("").trim().to_string();

    println!("cargo:rustc-link-search=native={}", root.join("build/shim-macos").display());
    println!("cargo:rustc-link-lib=static=yw_all");
    for f in frameworks.split(',').filter(|f| !f.is_empty()) {
        println!("cargo:rustc-link-lib=framework={f}");
    }
}
