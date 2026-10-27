//! Read-only check of the local gateway through the console client:
//! `cargo run -p eavs-console-core --example inspect`.
use eavs_console_core::{Client, instances};

fn main() -> anyhow::Result<()> {
    let path = instances::eavs_config_path().ok_or_else(|| anyhow::anyhow!("no config path"))?;
    let local = instances::discover_local(&path).ok_or_else(|| anyhow::anyhow!("no local eavs"))?;
    println!("instance {} at {}", local.name, local.url);
    let client = Client::connect(&local)?;
    match client.sources() {
        Ok(sources) => println!("providers: {}", sources.len()),
        Err(e) => println!("providers: {e}"),
    }
    match client.availability() {
        Ok(a) => println!(
            "availability: {} providers, {} with deactivations",
            a.providers.len(),
            a.availability.len()
        ),
        Err(e) => println!("availability: {e}"),
    }
    match client.usage(false) {
        Ok(u) => println!("usage accounts: {}", u.len()),
        Err(e) => println!("usage: {e}"),
    }
    Ok(())
}
