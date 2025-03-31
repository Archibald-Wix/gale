use std::path::PathBuf;

use eyre::{anyhow, Context, OptionExt, Result};
use log::{error, info};
use serde_json::Value;
use tauri::App;
use tauri_plugin_cli::CliExt;

use crate::{
    game::{self},
    profile::{self, install::InstallOptions},
    state::ManagerExt,
};

pub fn run(app: &App) -> Result<()> {
    match app.cli().matches() {
        Ok(matches) => {
            if matches.args.is_empty() {
                return Ok(());
            }

            let mut manager = app.lock_manager();

            if let Some(Value::String(slug)) = matches.args.get("game").map(|arg| &arg.value) {
                let game = game::from_slug(slug).ok_or_eyre("unknown game id")?;

                manager
                    .set_active_game(game, app.handle())
                    .context("failed to set game")?;

                manager.save_all(app.db())?;
            }

            if let Some(Value::String(profile)) = matches.args.get("profile").map(|arg| &arg.value)
            {
                let game = manager.active_game_mut();
                let index = game.profile_index(profile).ok_or_eyre("unknown profile")?;

                game.set_active_profile(index)
                    .context("failed to set profile")?;

                game.save(app.db())?;
            }

            let handle = match matches.args.get("install").map(|arg| &arg.value) {
                Some(Value::String(path)) => {
                    let path = PathBuf::from(path);
                    let handle = app.handle().to_owned();

                    Some(tauri::async_runtime::spawn(install_local_mod(path, handle)))
                }
                _ => None,
            };

            if let Some(Value::Bool(true)) = matches.args.get("launch").map(|arg| &arg.value) {
                manager
                    .active_game()
                    .launch(&app.lock_prefs(), app.handle())
                    .context("failed to launch game")?;
            }

            if let Some(Value::Bool(true)) = matches.args.get("no-gui").map(|arg| &arg.value) {
                if let Some(handle) = handle {
                    tauri::async_runtime::spawn(async move {
                        handle.await.ok();
                        std::process::exit(0);
                    });
                } else {
                    std::process::exit(0);
                }
            }

            Ok(())
        }
        Err(err) => Err(anyhow!(err)),
    }
}

async fn install_local_mod(path: PathBuf, handle: tauri::AppHandle) {
    profile::import::import_local_mod(
        path,
        None,
        &handle,
        InstallOptions::default().on_progress(Box::new(|progress, _| {
            info!(
                "{} {} ({}%)",
                progress.task,
                progress.current_name,
                (progress.total_progress * 100.0).round()
            )
        })),
    )
    .await
    .unwrap_or_else(|err| error!("failed to install mod from cli: {:#}", err));
}
