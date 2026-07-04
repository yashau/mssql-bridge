fn main() {
    let version = std::env::var("MSSQL_BRIDGE_VERSION")
        .ok()
        .or_else(github_ref_version)
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set"));

    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    println!("cargo:rerun-if-env-changed=MSSQL_BRIDGE_VERSION");
    println!("cargo:rustc-env=MSSQL_BRIDGE_VERSION={version}");
}

fn github_ref_version() -> Option<String> {
    let tag = std::env::var("GITHUB_REF_NAME").ok()?;
    let version = tag.strip_prefix('v').unwrap_or(&tag);
    is_calver(version).then(|| version.to_string())
}

fn is_calver(version: &str) -> bool {
    let parts: Vec<&str> = version.split('-').collect();
    if parts.len() != 4 {
        return false;
    }
    parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| part.chars().all(|c| c.is_ascii_digit()))
        && parts[3].parse::<u32>().is_ok_and(|sequence| sequence > 0)
}
