use std::{fmt::Display, io::Cursor};

use chrono::{DateTime, Utc};
use eyre::{bail, eyre, Context, ContextCompat, OptionExt, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{profile::install::InstallOptions, state::ManagerExt};

use super::export;

pub mod auth;
pub mod commands;

const API_URL: &str = "http://localhost:8800/api";

async fn request(method: Method, path: impl Display, app: &AppHandle) -> reqwest::RequestBuilder {
    let mut req = app.http().request(method, format!("{}{}", API_URL, path));
    if let Some(token) = auth::access_token(app).await {
        req = req.bearer_auth(token);
    }
    req
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSyncProfileResponse {
    id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncProfileMetadata {
    id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    owner: auth::User,
    manifest: export::LegacyProfileManifest,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SyncProfileData {
    id: String,
    owner: auth::User,
    synced_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<SyncProfileMetadata> for SyncProfileData {
    fn from(value: SyncProfileMetadata) -> Self {
        SyncProfileData {
            id: value.id,
            owner: value.owner,
            synced_at: value.updated_at,
            updated_at: value.updated_at,
        }
    }
}

async fn create_profile(app: &AppHandle) -> Result<String> {
    let Some(user) = auth::user_info(app) else {
        bail!("not logged in");
    };

    let bytes = {
        let manager = app.lock_manager();
        let game = manager.active_game();
        let profile = game.active_profile();

        let mut bytes = Cursor::new(Vec::new());
        super::export::export_zip(&profile, &mut bytes, game.game)
            .context("failed to export profile")?;

        bytes.into_inner()
    };

    let response: CreateSyncProfileResponse = request(Method::POST, "/profile", app)
        .await
        .body(bytes)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let id = response.id.clone();

    {
        let mut manager = app.lock_manager();
        let profile = manager.active_profile_mut();

        profile.sync_profile = Some(SyncProfileData {
            id: id.clone(),
            owner: user,
            synced_at: response.updated_at,
            updated_at: response.updated_at,
        });

        profile.save(app.db())?;
    }

    Ok(id)
}

async fn push_profile(app: &AppHandle) -> Result<()> {
    let (id, bytes) = {
        let manager = app.lock_manager();
        let game = manager.active_game();
        let profile = game.active_profile();

        let id = profile
            .sync_profile
            .as_ref()
            .map(|data| data.id.clone())
            .ok_or_eyre("profile is not synced")?;

        let mut bytes = Cursor::new(Vec::new());
        super::export::export_zip(&profile, &mut bytes, game.game)
            .context("failed to export profile")?;

        (id, bytes.into_inner())
    };

    let response: CreateSyncProfileResponse = request(Method::PUT, format!("/profile/{id}"), app)
        .await
        .body(bytes)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    {
        let mut manager = app.lock_manager();
        let profile = manager.active_profile_mut();
        let sync_data = profile.sync_profile.as_mut().unwrap();

        sync_data.synced_at = response.updated_at;
        sync_data.updated_at = response.updated_at;

        profile.save(&app.db())?;
    };

    Ok(())
}

async fn clone_profile(id: String, app: &AppHandle) -> Result<()> {
    let metadata = get_profile_meta(id, app)
        .await?
        .ok_or_eyre("profile not found")?;

    let name = format!("{} (client)", metadata.manifest.profile_name);
    download_and_import_file(name, metadata.into(), app).await
}

pub async fn pull_profile(dry_run: bool, app: &AppHandle) -> Result<()> {
    let (id, name, synced_at) = {
        let mut manager = app.lock_manager();
        let profile = manager.active_profile_mut();

        match &profile.sync_profile {
            Some(data) => (data.id.clone(), profile.name.clone(), data.synced_at),
            None => bail!("profile is not synced"),
        }
    };

    let metadata = get_profile_meta(id, app).await?;

    match metadata {
        Some(metadata) if !dry_run && metadata.updated_at > synced_at => {
            download_and_import_file(name, metadata.into(), app).await
        }
        _ => {
            let mut manager = app.lock_manager();
            let profile = manager.active_profile_mut();

            let synced_at = profile.sync_profile.take().unwrap().synced_at;

            profile.sync_profile = metadata.map(|metadata| SyncProfileData {
                synced_at,
                ..metadata.into()
            });

            Ok(())
        }
    }
}

async fn download_and_import_file(
    name: String,
    sync_profile: SyncProfileData,
    app: &AppHandle,
) -> Result<()> {
    let path = format!("/profile/{}", sync_profile.id);
    let bytes = request(Method::GET, path, app)
        .await
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let mut data =
        super::import::read_file(Cursor::new(bytes), app).context("failed to import profile")?;

    data.name = name.clone();

    super::import::import_profile(data, InstallOptions::default(), false, app)
        .await
        .context("failed to import profile")?;

    {
        // import_data deletes and recreates the profile, so we need to set sync_data again
        let mut manager = app.lock_manager();

        let game = manager.active_game_mut();
        let index = game.profile_index(&name).context("profile not found")?;
        let profile = &mut game.profiles[index];

        profile.sync_profile = Some(sync_profile);

        profile.save(app.db())?;
        game.save(app.db())?;
    }

    Ok(())
}

async fn get_profile_meta(id: String, app: &AppHandle) -> Result<Option<SyncProfileMetadata>> {
    let res = request(Method::GET, format!("/profile/{id}/meta"), app)
        .await
        .send()
        .await?
        .error_for_status();

    match res {
        Ok(res) => {
            let res = res.json().await?;
            Ok(Some(res))
        }
        Err(err) if err.status() == Some(StatusCode::NOT_FOUND) => Ok(None),
        Err(err) => Err(eyre!(err)),
    }
}
