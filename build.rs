//! What the executable carries besides its code.
//!
//! Three things, all of them Windows resources compiled into the binary:
//!
//! * the **icon**, so the Start menu, the taskbar, Alt-Tab and the file's own
//!   entry in Explorer show something other than a blank page;
//! * the **version block**, which is what Apps & Features reads, what a support
//!   call means by "which version have you got", and what an installer uses to
//!   decide whether it is upgrading or downgrading;
//! * the **manifest**, which is the only way to say `PerMonitorV2` and
//!   `longPathAware` - both of them settings that cannot be made at run time
//!   because Windows has read them before `main` is entered.
//!
//! Nothing here runs off Windows, and nothing here is required for the program
//! to work when it does: a build with no resources is a working program with a
//! blank icon and a 260-character path limit. That is the reason every failure
//! below is a warning rather than a panic - a developer on a machine without
//! the resource compiler should still be able to run the tests.
//!
//! # Both binaries get all of it
//!
//! A build script runs once for the crate, not once per binary, so the resource
//! is linked into `files-cli` as well. That is the right answer rather than a
//! limitation worked around: `files-cli --doctor` walks the same shares the
//! panel does, and a console build *without* `longPathAware` would stop seeing
//! files at a depth the panel sees past - which is to say, the diagnostic tool
//! would disagree with the thing it is diagnosing.

fn main() {
    // Re-run only when the things actually embedded change. Without this the
    // resource compiler runs on every build of every file in the crate.
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

    // What Apps & Features and the file properties dialog show. `FileVersion`
    // comes from Cargo by default; the rest is what turns "files.exe" into
    // something a person can identify.
    res.set("ProductName", "files");
    res.set(
        "FileDescription",
        "Search job codes across the drawing shares",
    );
    res.set("CompanyName", "files");
    res.set("LegalCopyright", "");
    res.set("OriginalFilename", "files.exe");
    res.set("InternalName", "files");

    if let Err(err) = res.compile() {
        // A warning rather than a failure. See the module note: a build without
        // the resource compiler is a working program with a blank icon, and
        // refusing to build at all would be the larger problem.
        println!("cargo:warning=no icon or manifest was embedded: {err}");
    }
}
