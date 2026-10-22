fn main() {
    #[cfg(windows)]
    {
        embed_resource::compile("assets/icon.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
