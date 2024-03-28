use crate::cli::clapargs::EmptyArgs;

/// Print the version
pub async fn version(_args: EmptyArgs) -> Result<(), Box<dyn std::error::Error>> {
    // git version 2.44.0
    println!(
        "{} version {}",
        env!("CARGO_PKG_NAME").to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    Ok(())
}
