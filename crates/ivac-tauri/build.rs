fn main() {
    // Bake the git-describe identity into the binary so the `version`
    // command's `git_sha` field is populated without external env
    // plumbing. Mirrors the frontend's `__IVAC_BUILD_VERSION__` vite
    // define (git describe --always --dirty). Falls back to leaving
    // GIT_SHA unset (option_env! → None) when git or the repo is
    // unavailable, e.g. building from a source tarball.
    if std::env::var_os("GIT_SHA").is_none() {
        if let Ok(out) = std::process::Command::new("git")
            .args(["describe", "--always", "--dirty"])
            .output()
        {
            if out.status.success() {
                let desc = String::from_utf8_lossy(&out.stdout);
                let desc = desc.trim();
                if !desc.is_empty() {
                    println!("cargo:rustc-env=GIT_SHA={desc}");
                }
            }
        }
    }
    // Re-run when HEAD moves so the baked identity tracks checkouts.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    tauri_build::build();
}
