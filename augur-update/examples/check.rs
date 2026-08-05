//! Manual smoke test against the live release feed.
//!
//!     cargo run -p augur-update --example check -- [current-version]
//!
//! Not part of the test suite on purpose: it makes a real network request, and
//! its result depends on what is published right now. Use it to confirm the
//! updater sees a staged release before shipping one.

fn main() {
    let current = std::env::args()
        .nth(1)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());

    println!("repo:    {}", augur_update::releases_url());
    println!("running: {current}");
    match augur_update::install_kind() {
        Ok(kind) => println!("install: {kind}"),
        Err(error) => println!("install: not updatable in place ({error})"),
    }

    match augur_update::check(&current) {
        Ok(augur_update::UpdateStatus::UpToDate(version)) => {
            println!("result:  up to date at {version}");
        }
        Ok(augur_update::UpdateStatus::Available(release)) => {
            println!("result:  {} available", release.version);
            println!(
                "  asset: {} ({} bytes)",
                release.asset.name, release.asset.size
            );
            println!("  notes: {}", release.notes_url);
        }
        Err(error) => println!("result:  {error}"),
    }
}
