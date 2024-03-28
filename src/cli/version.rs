use crate::cli::clapargs::EmptyArgs;

/// Print the version
pub async fn version(_args: EmptyArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}\n", env!("CARGO_PKG_VERSION").to_string());
    Ok(())
}
