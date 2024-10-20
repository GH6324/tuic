#![feature(trivial_bounds)]
#![feature(let_chains)]

use std::{env, process};

use chrono::{Local, Offset, TimeZone};
use config::{Config, parse_config};
use lateinit::LateInit;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{old_config::ConfigError, server::Server};

mod config;
mod connection;
mod error;
mod old_config;
mod restful;
mod server;
mod utils;

pub static CONFIG: LateInit<Config> = LateInit::new();

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = match parse_config(env::args_os()).await {
        Ok(cfg) => cfg,
        Err(ConfigError::Version(msg) | ConfigError::Help(msg)) => {
            println!("{msg}");
            process::exit(0);
        }
        Err(err) => {
            eprintln!("{err}");
            process::exit(1);
        }
    };
    unsafe {
        CONFIG.init(cfg);
    }
    let filter = tracing_subscriber::filter::Targets::new().with_default(CONFIG.log_level);
    let registry = tracing_subscriber::registry();
    registry
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_timer(tracing_subscriber::fmt::time::OffsetTime::new(
                    time::UtcOffset::from_whole_seconds(
                        Local
                            .timestamp_opt(0, 0)
                            .unwrap()
                            .offset()
                            .fix()
                            .local_minus_utc(),
                    )
                    .unwrap_or(time::UtcOffset::UTC),
                    time::macros::format_description!(
                        "[year repr:last_two]-[month]-[day] [hour]:[minute]:[second]"
                    ),
                )),
        )
        .try_init()?;
    tokio::spawn(async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for event");
    });
    match Server::init() {
        Ok(server) => server.start().await,
        Err(err) => {
            eprintln!("{err}");
            process::exit(1);
        }
    }
    Ok(())
}
