fn main() {
    println!("cargo:rerun-if-changed=assets/noctiluca.ico");
    println!("cargo:rerun-if-changed=assets/noctiluca.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/noctiluca.rc", embed_resource::NONE)
            .manifest_required()
            .expect("无法嵌入 Noctiluca Windows 图标");
    }
}
