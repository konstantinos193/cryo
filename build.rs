fn main() {
    #[cfg(windows)]
    {
        // embed-resource emits no `rerun-if-changed` of its own, so name the
        // inputs here or an edited icon never gets relinked.
        println!("cargo:rerun-if-changed=assets/icon.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
        embed_resource::compile("assets/icon.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
