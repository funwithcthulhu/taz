#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use log::info;
use taz_reader::gui;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .init();

    info!("{} starting", env!("CARGO_PKG_NAME"));
    gui::run().context("failed to launch GUI")
}
