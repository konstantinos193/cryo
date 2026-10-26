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

        // Unsigned binaries with no VERSIONINFO get a harder SmartScreen
        // look, and users checking Properties see a blank file. Fill the
        // resource from Cargo metadata so it never drifts from the crate
        // version.
        let version = std::env::var("CARGO_PKG_VERSION").unwrap();
        let mut nums = version
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u16>().unwrap_or(0));
        let file_version = format!(
            "{},{},{},0",
            nums.next().unwrap_or(0),
            nums.next().unwrap_or(0),
            nums.next().unwrap_or(0)
        );
        let rc = format!(
            r#"1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
FILEOS 0x40004
FILEFLAGSMASK 0x3F
FILEFLAGS 0
FILETYPE 0x1
FILESUBTYPE 0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "konstantinos193"
            VALUE "FileDescription", "cryo browser"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "cryo"
            VALUE "LegalCopyright", "MIT License"
            VALUE "OriginalFilename", "cryo.exe"
            VALUE "ProductName", "cryo"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 0x04B0
    END
END
"#
        );
        let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("version.rc");
        std::fs::write(&out, rc).unwrap();
        println!("cargo:rerun-if-changed=Cargo.toml");
        embed_resource::compile(&out, embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
