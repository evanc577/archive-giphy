use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::StreamExt;
use serde::Deserialize;
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Parser, Debug)]
struct Args {
    /// Giphy member ID
    #[clap(short, long)]
    member: u64,

    /// Download directory
    #[clap(short, long)]
    directory: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let gifs = gifs(&client, args.member).await?;
    download(&client, gifs, args.directory).await?;

    Ok(())
}

#[derive(Deserialize, Debug)]
struct GiphyResponse {
    next: Option<String>,
    results: Vec<GiphyGif>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct GiphyGif {
    id: String,
    index_id: u64,
    images: HashMap<String, serde_json::Value>,
    title: String,
    user: GiphyUser,
    #[serde(rename = "create_datetime")]
    create_time: String,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct GiphyUser {
    id: u64,
    name: String,
    username: String,
}

#[derive(Error, Debug)]
enum GiphyError {
    #[error("Received response error status {code} for url {url}")]
    ResponseError { code: u16, url: String },
    #[error("No source video found")]
    NoSourceVideo,
    #[error("Invalid date {date}")]
    InvalidDate { date: String },
}

async fn gifs(client: &reqwest::Client, member_id: u64) -> Result<Vec<GiphyGif>> {
    let mut gifs = Vec::new();
    let mut url = format!("https://giphy.com/api/v4/channels/{}/feed", member_id);

    for i in 1.. {
        println!("Fetching page {}", i);

        // Query GIFs
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(GiphyError::ResponseError {
                code: resp.status().as_u16(),
                url: url.clone(),
            });
        }

        // Append GIFs
        let text = resp.text().await?;
        let mut giphy_resp: GiphyResponse = serde_json::from_str(&text)?;
        gifs.append(&mut giphy_resp.results);

        // Check for more
        match giphy_resp.next {
            Some(u) => url = u,
            None => break,
        }
    }

    Ok(gifs)
}

async fn download(
    client: &reqwest::Client,
    gifs: Vec<GiphyGif>,
    dir: impl AsRef<Path>,
) -> Result<()> {
    let results =
        futures::stream::iter(gifs.into_iter().map(|gif| download_gif(client, gif, &dir)))
            .buffer_unordered(20)
            .collect::<Vec<_>>()
            .await;
    for r in results {
        if let Err(e) = r {
            eprintln!("Failed to download {}", e);
            e.chain().skip(1).for_each(|cause| eprintln!("  {}", cause));
        }
    }

    Ok(())
}

async fn download_gif(
    client: &reqwest::Client,
    gif: GiphyGif,
    base_dir: impl AsRef<Path>,
) -> Result<()> {
    let id = gif.id.clone();
    _download_gif(client, gif, base_dir)
        .await
        .context(format!("id: {}", id))
}

async fn _download_gif(
    client: &reqwest::Client,
    gif: GiphyGif,
    base_dir: impl AsRef<Path>,
) -> Result<()> {
    // Get source url
    let source_url = gif
        .images
        .get("source")
        .ok_or(GiphyError::NoSourceVideo)?
        .get("url")
        .ok_or(GiphyError::NoSourceVideo)?
        .as_str()
        .ok_or(GiphyError::NoSourceVideo)?;
    let ext = source_url
        .rsplit_once('.')
        .ok_or(GiphyError::NoSourceVideo)?
        .1;

    // Generate file name and create directory
    let date = gif
        .create_time
        .split_once('T')
        .ok_or_else(|| GiphyError::InvalidDate {
            date: gif.create_time.clone(),
        })?
        .0
        .replace('-', "");
    let filename = format!(
        "{}_{}_{:012}_{}.{}",
        date, &gif.user.username, &gif.index_id, &gif.id, ext
    );
    let dir = base_dir.as_ref().join(&gif.user.username);
    fs::create_dir_all(&dir).await?;
    let path = dir.join(filename);

    // Check if file exists
    if path.exists() {
        return Ok(());
    }

    // Download
    let video = client.get(source_url).send().await?.bytes().await?;
    let mut buffer = fs::File::create(&path).await?;
    buffer.write_all(&video).await?;

    println!("Downloaded {}", path.to_string_lossy());

    Ok(())
}
