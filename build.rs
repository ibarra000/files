//! Embeds the Windows resources: the icon, the version block (read by Apps &
//! Features and the installer's upgrade check) and the manifest, which sets
//! `PerMonitorV2` and `longPathAware` before `main` runs.
//!
//! Resources are optional, so failures are warnings: without them the program
//! still works, with a blank icon and a 260-character path limit.
//!
//! Both binaries receive the resources. `files-cli --doctor` must also be
//! `longPathAware`, or it would miss files the panel can see.

fn main() {
    // Without these, the resource compiler reruns on every crate change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/files.ico");
    println!("cargo:rerun-if-changed=assets/files.manifest");

    #[cfg(windows)]
    embed();
}

#[cfg(windows)]
fn embed() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/files.ico");
    res.set_manifest_file("assets/files.manifest");

    // Shown by Apps & Features and the file properties dialog. `FileVersion`
    // comes from Cargo.
    res.set("ProductName", "files");
    res.set("FileDescription", env!("CARGO_PKG_DESCRIPTION"));
    res.set("CompanyName", "files");
    res.set("LegalCopyright", "");
    res.set("OriginalFilename", "files.exe");
    res.set("InternalName", "files");

    if let Err(err) = res.compile() {
        // Resources are optional; see the module docs.
        println!("cargo:warning=no icon or manifest was embedded: {err}");
    }
}
