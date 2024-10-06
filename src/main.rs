use std::{io, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::Context;
use clap::Parser;
use futures::future::try_join_all;
use serde::Deserialize;
use tokio::{
    fs::{self, set_permissions},
    io::{AsyncSeekExt, AsyncWriteExt},
    time::{sleep, Instant},
};
use tracing::{error, info};
use valuable::Valuable;

#[derive(Parser)]
struct Opts {
    #[clap(short, long, env, default_value = "/etc/ssh_key_sync/config.toml")]
    config: PathBuf,
}

#[derive(Deserialize)]
struct Config {
    authorized_keys_file: PathBuf,
    github_users: Vec<String>,
    #[serde(with = "humantime_serde")]
    sync_duration: std::time::Duration,
}

async fn run(opts: &Opts) -> anyhow::Result<()> {
    let config = fs::read_to_string(&opts.config)
        .await
        .with_context(|| "read config file")?;
    let config: Config = toml::from_str(&config).with_context(|| "parse config file")?;

    if let Some(parent) = config.authorized_keys_file.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| "create ssh dir")?;
        set_permissions(parent, PermissionsExt::from_mode(0o700))
            .await
            .with_context(|| "set mode")?;
    }

    let mut authorized_keys = fs::File::options()
        .append(false)
        .create(true)
        .truncate(true)
        .write(true)
        .open(&config.authorized_keys_file)
        .await
        .with_context(|| "open authorized keys")?;
    set_permissions(
        &config.authorized_keys_file,
        PermissionsExt::from_mode(0o600),
    )
    .await
    .with_context(|| "set mode")?;
    loop {
        let instant = Instant::now();

        info!(users = config.github_users.as_value(), "try sync");
        let keys = config.github_users.iter().map(|user| async move {
            let keys = reqwest::get(format!("https://github.com/{user}.keys"))
                .await
                .with_context(|| "get ssh keys")?
                .text()
                .await
                .with_context(|| "decode got response")?;
            Ok::<_, anyhow::Error>(format!("# {user}\n{keys}\n"))
        });
        let keys = try_join_all(keys).await?.join("\n");
        authorized_keys
            .seek(io::SeekFrom::Start(0))
            .await
            .with_context(|| "seek to start")?;
        authorized_keys
            .set_len(0)
            .await
            .with_context(|| "truncate authorized_keys")?;
        authorized_keys
            .write_all(keys.as_bytes())
            .await
            .with_context(|| "write authorized keys")?;
        authorized_keys
            .flush()
            .await
            .with_context(|| "flush authorized keys")?;

        let elapsed = instant.elapsed();
        if elapsed < config.sync_duration {
            sleep(config.sync_duration - elapsed).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let opts = Opts::parse();
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    tracing_subscriber::registry()
        .with(fmt::layer().with_ansi(false))
        .with(EnvFilter::from_default_env())
        .init();

    if let Err(e) = run(&opts).await {
        error!("{e}");
    }
}
