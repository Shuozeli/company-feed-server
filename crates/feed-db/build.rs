fn main() {
    // `sqlx::migrate!` embeds migrations at compile time. Make migration-only
    // changes invalidate this crate in local and container release builds.
    println!("cargo:rerun-if-changed=../../migrations");
}
