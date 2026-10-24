use std::time::Duration;

use circus_sandbox::environment::{ImagePlatform, ImageRequest, prepare_docker_image};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reference =
        std::env::var("CIRCUS_DOCKER_IMAGE").unwrap_or_else(|_| "python:3.12-slim".into());
    let platform = ImagePlatform::new("linux", std::env::var("CIRCUS_DOCKER_ARCH")?)?;
    // Resolution is local-only: a missing image is a typed error and is never pulled.
    let image = prepare_docker_image(ImageRequest::new(
        std::env::var("CIRCUS_DOCKER_ENDPOINT")?,
        reference,
        platform,
        Duration::from_secs(120),
    )?)
    .await?;
    println!("{} {}/{}", image.id(), image.os(), image.architecture());
    Ok(())
}
