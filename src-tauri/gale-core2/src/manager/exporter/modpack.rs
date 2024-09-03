use anyhow::{anyhow, ensure, Context, Result};
use futures_util::future::try_join_all;
use image::{imageops::FilterType, ImageFormat};
use log::{debug, info};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tauri::Url;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
};
use uuid::Uuid;

use std::{
    collections::HashMap,
    fmt::Display,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    manager::Profile,
    thunderstore::{
        models::{
            CompletedPart, PackageManifest, PackageSubmissionMetadata, UploadPartUrl,
            UserMediaFinishUploadParams, UserMediaInitiateUploadParams,
            UserMediaInitiateUploadResponse,
        },
        ModRef, Thunderstore,
    },
    util,
};

pub mod changelog;

pub fn refresh_args(profile: &mut Profile) {
    if profile.modpack.is_none() {
        profile.modpack = Some(ModpackArgs {
            name: profile.name.replace([' ', '-'], ""),
            readme: format!("# {}\n\n", profile.name),
            changelog: "# Changelog\n\n## 1.0.0\n\n- Initial release".to_owned(),
            version_number: "1.0.0".to_owned(),
            categories: vec!["modpacks".to_owned()],
            ..Default::default()
        });
    }

    let includes = &mut profile.modpack.as_mut().unwrap().include_files;

    // remove deleted files
    includes.retain(|file, _| profile.path.join(file).exists());

    for path in super::find_includes(&profile.path) {
        includes.entry(path).or_insert(true);
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModpackArgs {
    pub name: String,
    pub description: String,
    pub author: String,
    pub categories: Vec<String>,
    pub nsfw: bool,
    pub readme: String,
    #[serde(default)]
    pub changelog: String,
    pub version_number: String,
    pub icon_path: PathBuf,
    pub website_url: String,
    pub include_disabled: bool,
    #[serde(default, rename = "includeFileMap")]
    pub include_files: HashMap<PathBuf, bool>,
}

impl Profile {
    fn mods_to_pack<'a>(&'a self, args: &'a ModpackArgs) -> impl Iterator<Item = &'a ModRef> + 'a {
        self.remote_mods()
            .filter(move |(_, _, enabled)| args.include_disabled || *enabled)
            .map(|(mod_ref, _, _)| mod_ref)
    }

    pub fn export_pack(
        &self,
        args: &ModpackArgs,
        writer: impl Write + Seek,
        thunderstore: &Thunderstore,
    ) -> Result<()> {
        ensure!(!args.name.is_empty(), "name cannot be empty");
        ensure!(!args.description.is_empty(), "description cannot be empty");

        let deps = self
            .mods_to_pack(args)
            .map(|mod_ref| {
                let borrowed = mod_ref.borrow(thunderstore)?;
                Ok(borrowed.version.full_name.clone())
            })
            .collect::<Result<Vec<_>>>()
            .context("failed to resolve modpack dependencies")?;

        let version_number =
            semver::Version::parse(&args.version_number).context("invalid version number")?;

        let manifest = PackageManifest {
            name: args.name.clone(),
            description: args.description.clone(),
            website_url: args.website_url.clone(),
            dependencies: deps,
            installers: None,
            author: None,
            version_number,
        };

        let mut zip = util::zip::builder(writer)?;

        if !args.readme.is_empty() {
            zip.write_str("README.md", &args.readme)?;
        }

        if !args.changelog.is_empty() {
            zip.write_str("CHANGELOG.md", &args.changelog)?;
        }

        serde_json::to_writer_pretty(zip.writer("manifest.json")?, &manifest)?;

        write_icon(&args.icon_path, &mut zip).context("failed to write icon")?;

        super::write_includes(
            args.include_files
                .iter()
                .filter(|(_, enabled)| **enabled)
                .map(|(file, _)| file),
            &self.path,
            &mut zip,
        )?;

        Ok(())
    }
}

fn write_icon<W>(path: &Path, zip: &mut util::zip::ZipBuilder<W>) -> anyhow::Result<()>
where
    W: Write + Seek,
{
    let img = image::ImageReader::open(path)?.decode()?;
    let img = img.resize_exact(256, 256, FilterType::Lanczos3);

    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)?;
    zip.write("icon.png", &bytes)?;

    Ok(())
}

fn base_request(
    tail: impl Display,
    token: impl Display,
    client: &reqwest::Client,
) -> reqwest::RequestBuilder {
    let url = format!("https://thunderstore.io/api/experimental/{}/", tail);

    client.post(url).bearer_auth(token)
}

pub async fn publish(
    path: PathBuf,
    game_id: &str,
    args: ModpackArgs,
    token: String,
    client: reqwest::Client,
) -> Result<()> {
    ensure!(args.description.len() <= 250, "description is too long");
    ensure!(!args.readme.is_empty(), "readme cannot be empty");
    ensure!(!args.author.is_empty(), "author cannot be empty");

    if !args.website_url.is_empty() {
        Url::parse(&args.website_url).context("invalid website URL")?;
    }

    info!("publishing modpack");

    let response = initiate_upload(&path, &token, &client)
        .await
        .context("failed to initiate upload")?;

    let uuid = response.user_media.uuid.context("no uuid in response")?;

    let path = Arc::new(path);

    let tasks = response.upload_urls.into_iter().map(|part| {
        let path = path.clone();
        let client = client.clone();
        tauri::async_runtime::spawn(upload_chunk(part, path, client))
    });

    let parts = match try_join_all(tasks)
        .await
        .map_err(|err| anyhow!(err))
        .and_then(|parts| parts.into_iter().collect::<Result<Vec<_>>>())
    {
        Ok(parts) => parts,
        Err(err) => {
            tauri::async_runtime::spawn(async move { abort_upload(&uuid, &token, client).await });
            return Err(err.context("failed to upload file"));
        }
    };

    finish_upload(parts, &uuid, &token, &client)
        .await
        .context("failed to finalize upload")?;

    submit_package(uuid, game_id, args, &token, &client)
        .await
        .context("failed to submit package")?;

    Ok(())
}

async fn initiate_upload(
    path: &Path,
    token: &str,
    client: &reqwest::Client,
) -> Result<UserMediaInitiateUploadResponse> {
    let name = util::fs::file_name_owned(path);
    let size = path.metadata()?.len();

    debug!(
        "initiating modpack upload for {}, size: {} bytes",
        name, size
    );

    let response = base_request("usermedia/initiate-upload", token, client)
        .json(&UserMediaInitiateUploadParams {
            filename: name,
            file_size_bytes: size,
        })
        .send()
        .await?
        .map_auth_err()?
        .json::<UserMediaInitiateUploadResponse>()
        .await?;

    debug!("recieved {} upload urls", response.upload_urls.len());

    Ok(response)
}

async fn upload_chunk(
    part: UploadPartUrl,
    path: Arc<PathBuf>,
    client: reqwest::Client,
) -> Result<CompletedPart> {
    let mut file = fs::File::open(&*path).await?;

    file.seek(SeekFrom::Start(part.offset)).await?;

    let mut buffer = Vec::with_capacity(part.length as usize);
    file.take(part.length).read_to_end(&mut buffer).await?;

    let response = client
        .put(&part.url)
        .body(buffer)
        .send()
        .await?
        .error_for_status()?;

    let tag = response
        .headers()
        .get("ETag")
        .context("no ETag in response")?
        .to_str()
        .context("ETag is not valid utf-8")?
        .to_owned();

    debug!("uploaded part {} with tag {}", part.part_number, tag);

    Ok(CompletedPart {
        tag,
        part_number: part.part_number,
    })
}

async fn abort_upload(uuid: &Uuid, token: &str, client: reqwest::Client) -> Result<()> {
    info!("aborting upload");

    base_request(format!("usermedia/{}/abort-upload", uuid), token, &client)
        .json(&uuid)
        .send()
        .await?
        .map_auth_err()?;

    Ok(())
}

async fn finish_upload(
    parts: Vec<CompletedPart>,
    uuid: &Uuid,
    token: &str,
    client: &reqwest::Client,
) -> Result<()> {
    debug!("finishing upload");

    base_request(format!("usermedia/{}/finish-upload", uuid), token, client)
        .json(&UserMediaFinishUploadParams { parts })
        .send()
        .await?
        .map_auth_err()?;

    Ok(())
}

async fn submit_package(
    uuid: Uuid,
    game_id: &str,
    args: ModpackArgs,
    token: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let metadata = PackageSubmissionMetadata {
        author_name: args.author,
        has_nsfw_content: args.nsfw,
        upload_uuid: uuid.to_string(),
        categories: Vec::new(),
        communities: vec![game_id.to_owned()],
        community_categories: HashMap::from([(game_id.to_owned(), args.categories)]),
    };

    debug!("submitting package");

    base_request("submission/submit", token, client)
        .json(&metadata)
        .send()
        .await?
        .map_auth_err_with(|status| match status {
            StatusCode::BAD_REQUEST => Some(anyhow!(
                "package metadata is invalid, please check your input"
            )),
            _ => None,
        })?;

    Ok(())
}

trait ReqwestResponseExt {
    fn map_auth_err_with<F>(self, f: F) -> anyhow::Result<reqwest::Response>
    where
        F: FnOnce(StatusCode) -> Option<anyhow::Error>;

    fn map_auth_err(self) -> anyhow::Result<reqwest::Response>;
}

impl ReqwestResponseExt for reqwest::Response {
    fn map_auth_err_with<F>(self, f: F) -> anyhow::Result<reqwest::Response>
    where
        F: FnOnce(StatusCode) -> Option<anyhow::Error>,
    {
        self.error_for_status().map_err(|err| match err.status() {
            Some(status) => match status {
                StatusCode::UNAUTHORIZED => anyhow!("thunderstore API token is invalid"),
                _ => match f(status) {
                    Some(err) => err,
                    None => anyhow!(err),
                },
            },
            None => anyhow!(err),
        })
    }

    fn map_auth_err(self) -> anyhow::Result<reqwest::Response> {
        self.map_auth_err_with(|_| None)
    }
}
