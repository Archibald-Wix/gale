use crate::ProfileModSource;
use anyhow::anyhow;
use futures_util::TryStreamExt;
use gale_core::prelude::*;
use serde::Serialize;
use sqlx::types::Json;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInfo {
    id: i64,
    name: String,
    path: String,
    community_id: i64,
    mods: Vec<ProfileModInfo>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProfileModInfo {
    id: i64,
    owner: Option<String>,
    name: String,
    version: String,
    index: i64,
    enabled: bool,
    href: Option<String>,
    kind: ProfileModKind,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum ProfileModKind {
    Thunderstore,
    Local,
    Github,
}

pub async fn single(id: i64, state: &AppState) -> Result<ProfileInfo> {
    let (name, path, community_id, community_slug) = sqlx::query!(
        "SELECT
            p.name,
            p.path,
            c.id,
            c.slug
        FROM
            profiles p
            JOIN communities c ON p.community_id = c.id
        WHERE p.id = ?
        ",
        id
    )
    .map(|record| (record.name, record.path, record.id, record.slug))
    .fetch_optional(&state.db)
    .await?
    .ok_or(anyhow!("profile not found"))?;

    let mut stream = sqlx::query!(
        r#"SELECT
            id,
            enabled,
            order_index,
            source AS "source: Json<ProfileModSource>"
        FROM profile_mods
        WHERE profile_id = ?"#,
        id
    )
    .fetch(&state.db);

    let mut mods = Vec::new();

    while let Some(record) = stream.try_next().await? {
        let (kind, owner, name, version, href) = match record.source.0 {
            ProfileModSource::Thunderstore { identifier, .. } => {
                let href = format!(
                    "{}/c/{}/p/{}/",
                    gale_thunderstore::api::THUNDERSTORE_URL,
                    community_slug,
                    identifier.path()
                );

                (
                    ProfileModKind::Thunderstore,
                    Some(identifier.owner().to_owned()),
                    identifier.name().to_owned(),
                    identifier.version().to_owned(),
                    Some(href),
                )
            }
            ProfileModSource::Local { full_name, version } => {
                let (owner, name) = match full_name.split_once('-') {
                    Some((owner, name)) => (Some(owner.to_owned()), name.to_owned()),
                    None => (None, full_name),
                };

                (ProfileModKind::Local, owner, name, version, None)
            }
            ProfileModSource::Github { owner, repo, tag } => {
                let href = format!("https://github.com/{owner}/{repo}/releases/tag/{tag}");

                (ProfileModKind::Github, Some(owner), repo, tag, Some(href))
            }
        };

        mods.push(ProfileModInfo {
            id: record.id,
            owner,
            name,
            version,
            index: record.order_index,
            enabled: record.enabled,
            href,
            kind,
        });
    }

    Ok(ProfileInfo {
        id,
        name,
        path,
        community_id,
        mods,
    })
}
