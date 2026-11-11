pub mod association;
mod github;
mod hub;
pub mod model;
mod package;
pub fn diagnose_repository(input: &str, root: std::path::PathBuf) -> anyhow::Result<()> {
    let paths = model::Paths { root };
    let source = github::resolve(input)?;
    let entries = github::catalog(&paths, &source, true)?;
    println!(
        "repository={} url={} compatible_latest_plugins={}",
        source.id,
        source.full_name,
        entries.len()
    );
    Ok(())
}
pub use hub::Hub;
pub fn smoke_hub(directory: std::path::PathBuf) -> anyhow::Result<()> {
    hub::smoke(directory)
}
pub use package::install_file;
