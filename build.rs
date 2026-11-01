fn main() {
    println!("cargo:rerun-if-changed=assets/gitstat.rc");
    println!("cargo:rerun-if-changed=assets/gitstat.ico");
    // The exe's icon, for Explorer and the Start menu shortcut, as oryx-win
    // does it. GPUI embeds the manifest itself.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/gitstat.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
