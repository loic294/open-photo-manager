fn main() {
    // `sqlx::migrate!` embeds the migration files at compile time but cannot
    // tell cargo about files added later; without this a new migration would not
    // trigger a rebuild.
    println!("cargo::rerun-if-changed=migrations");
}
