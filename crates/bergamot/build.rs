fn main() {
    let git_hash = read_git_hash().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BERGAMOT_GIT_HASH={git_hash}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/");
}

fn read_git_hash() -> Option<String> {
    let head = std::fs::read_to_string("../../.git/HEAD").ok()?;
    let head = head.trim();

    let full_hash = if let Some(ref_path) = head.strip_prefix("ref: ") {
        let ref_file = format!("../../.git/{ref_path}");
        std::fs::read_to_string(ref_file).ok()?
    } else {
        // Detached HEAD — already a hash
        head.to_string()
    };

    Some(full_hash.trim()[..7].to_string())
}
