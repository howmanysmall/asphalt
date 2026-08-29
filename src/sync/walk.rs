use crate::{
    asset::{self, Asset},
    cli::{SyncArgs, SyncTarget},
    config::{Config, Input, PackOptions},
    hash::Hash,
    lockfile::Lockfile,
    pack::Packer,
    sync::TargetBackend,
};
use anyhow::Context;
use fs_err::tokio as fs;
use log::{debug, info, warn};
use relative_path::{PathExt, RelativePathBuf};
use resvg::usvg::fontdb;
use std::{
    collections::{
        HashMap,
        hash_map::{self},
    },
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    sync::{Mutex, Semaphore, mpsc::UnboundedSender},
    task::JoinSet,
};
use walkdir::WalkDir;

pub struct Params {
    pub target: SyncTarget,
    pub existing_lockfile: Lockfile,
    pub font_db: Arc<fontdb::Database>,
    pub backend: Option<TargetBackend>,
    pub args: SyncArgs,
}

struct InputState {
    params: Arc<Params>,
    input_name: String,
    input_prefix: PathBuf,
    seen_hashes: Arc<Mutex<HashMap<Hash, PathBuf>>>,
    bleed: bool,
    optimize: bool,
    warn_each_duplicate: bool,
    pack_options: Option<PackOptions>,
    pending_sprites: Arc<Mutex<Vec<Asset>>>,
}

pub async fn walk(params: Params, config: &Config, tx: &UnboundedSender<super::Event>) {
    let params = Arc::new(params);

    for (input_name, input) in &config.inputs {
        let input_prefix = config.project_dir.join(input.include.get_prefix());

        let pack_options = if should_pack(input, &params.args) {
            Some(apply_pack_overrides(input.pack.as_ref(), &params.args))
        } else {
            None
        };

        let state = Arc::new(InputState {
            params: params.clone(),
            input_name: input_name.clone(),
            input_prefix: input_prefix.clone(),
            seen_hashes: Arc::new(Mutex::new(HashMap::new())),
            bleed: input.bleed,
            optimize: params.args.optimize,
            warn_each_duplicate: input.warn_each_duplicate,
            pack_options,
            pending_sprites: Arc::new(Mutex::new(Vec::new())),
        });

        let mut join_set = JoinSet::new();
        let semaphore = Arc::new(Semaphore::new(50));

        for entry in WalkDir::new(&input_prefix)
            .into_iter()
            .filter_entry(|entry| {
                let path = entry.path();
                if path == input_prefix {
                    return true;
                }
                if entry.file_type().is_dir() {
                    return true;
                }
                if let Ok(rel_path) = path.strip_prefix(&config.project_dir) {
                    input.include.is_match(rel_path)
                } else {
                    false
                }
            })
        {
            let Ok(entry) = entry else { continue };

            let path = entry.into_path();
            if !path.is_file() {
                continue;
            }
            let Some(ext) = path.extension() else {
                continue;
            };
            if !asset::is_supported_extension(ext) {
                continue;
            }

            let state = state.clone();
            let semaphore = semaphore.clone();
            let tx = tx.clone();

            tx.send(super::Event::Discovered(path.clone())).unwrap();

            join_set.spawn(async move {
                let _permit = semaphore.acquire_owned().await.unwrap();

                tx.send(super::Event::InFlight(path.clone())).unwrap();

                if let Err(e) = process_entry(state.clone(), &path, &tx).await {
                    warn!("Failed to process file {}: {e:?}", path.display());
                    tx.send(super::Event::Failed(path.clone())).unwrap();
                }
            });
        }

        while join_set.join_next().await.is_some() {}

        if let Some(pack_options) = &state.pack_options {
            if let Err(e) = pack_sprites(&state, pack_options, tx).await {
                warn!(
                    "Failed to pack sprites for input {}: {e:?}",
                    state.input_name
                );
                tx.send(super::Event::Failed(state.input_prefix.clone()))
                    .unwrap();
            }
        }
    }
}

async fn process_entry(
    state: Arc<InputState>,
    path: &Path,
    tx: &UnboundedSender<super::Event>,
) -> anyhow::Result<()> {
    debug!("Handling entry: {}", path.display());

    let rel_path = path.relative_to(&state.input_prefix)?;

    let data = fs::read(path).await?;

    let mut asset = Asset::new(rel_path.clone(), data.into()).context("Failed to create asset")?;

    let lockfile_entry = state
        .params
        .existing_lockfile
        .get(&state.input_name, &asset.hash);

    {
        let mut seen_hashes = state.seen_hashes.lock().await;

        match seen_hashes.entry(asset.hash) {
            hash_map::Entry::Occupied(entry) => {
                let seen_path = entry.get();
                let rel_seen_path = seen_path.relative_to(&state.input_prefix)?;

                debug!("Duplicate asset found: {} -> {}", rel_path, rel_seen_path);

                if state.warn_each_duplicate {
                    warn!(
                        "Duplicate file found: {} (original at {})",
                        rel_path, rel_seen_path
                    );
                }

                let event = super::Event::Finished {
                    state: super::EventState::Duplicate,
                    input_name: state.input_name.clone(),
                    path: path.into(),
                    rel_path: rel_path.clone(),
                    asset_ref: lockfile_entry.map(Into::into),
                    hash: asset.hash,
                    sprite_info: None,
                };
                tx.send(event).unwrap();

                return Ok(());
            }
            hash_map::Entry::Vacant(_) => {
                seen_hashes.insert(asset.hash, path.into());
            }
        }
    }

    // Packed inputs: images become part of an atlas instead of being synced directly
    let is_packed_image =
        state.pack_options.is_some() && matches!(asset.ty, asset::AssetType::Image(_));

    if is_packed_image && matches!(state.params.target, SyncTarget::Cloud { .. }) {
        if let Some(entry) = lockfile_entry {
            // This sprite was already uploaded as part of an atlas
            let event = super::Event::Finished {
                state: super::EventState::Synced { new: false },
                input_name: state.input_name.clone(),
                path: path.into(),
                rel_path: asset.path.clone(),
                hash: asset.hash,
                asset_ref: Some(entry.into()),
                sprite_info: entry.sprite_info.clone(),
            };
            tx.send(event).unwrap();

            return Ok(());
        }
    }

    let always_target = matches!(state.params.target, SyncTarget::Studio | SyncTarget::Debug);
    let is_new = always_target || lockfile_entry.is_none();

    if is_new {
        let font_db = state.params.font_db.clone();
        let bleed = state.bleed;
        let optimize = state.optimize;

        asset = tokio::task::spawn_blocking(move || -> anyhow::Result<Asset> {
            let mut asset = asset;
            asset.process(font_db, bleed, optimize)?;
            Ok(asset)
        })
        .await?
        .context("Failed to process asset")?;
    }

    if is_packed_image {
        state.pending_sprites.lock().await.push(asset);
        return Ok(());
    }

    let asset_ref = match state.params.backend {
        Some(ref backend) => backend.sync(&asset, lockfile_entry).await?,
        None => lockfile_entry.map(Into::into),
    };

    let event = super::Event::Finished {
        state: super::EventState::Synced { new: is_new },
        input_name: state.input_name.clone(),
        path: path.into(),
        rel_path: asset.path.clone(),
        hash: asset.hash,
        asset_ref,
        sprite_info: None,
    };
    tx.send(event).unwrap();

    Ok(())
}

async fn pack_sprites(
    state: &Arc<InputState>,
    pack_options: &PackOptions,
    tx: &UnboundedSender<super::Event>,
) -> anyhow::Result<()> {
    let sprites = {
        let mut pending = state.pending_sprites.lock().await;
        std::mem::take(&mut *pending)
    };

    if sprites.is_empty() {
        return Ok(());
    }

    info!(
        "Packing {} images for input '{}'",
        sprites.len(),
        state.input_name
    );

    let packer = Packer::new(pack_options.clone());
    let pack_result = packer.pack_assets(&sprites, &state.input_name)?;

    if pack_result.atlases.is_empty() {
        warn!("No atlases were generated for input '{}'", state.input_name);
        return Ok(());
    }

    // Map sprite names to their original absolute path, relative path, and hash
    let mut sprite_meta: HashMap<String, (PathBuf, RelativePathBuf, Hash)> = HashMap::new();
    for sprite in &sprites {
        if let Some(name) = sprite.path.file_stem() {
            sprite_meta.insert(
                name.to_string(),
                (
                    sprite.path.to_logical_path(&state.input_prefix),
                    sprite.path.clone(),
                    sprite.hash,
                ),
            );
        }
    }

    let atlas_count = pack_result.atlases.len();

    for atlas in &pack_result.atlases {
        let filename = format!("{}-sheet-{}.png", state.input_name, atlas.page_index);
        let atlas_asset = Asset::new(
            RelativePathBuf::from(filename),
            atlas.image_data.clone().into(),
        )
        .context("Failed to create atlas asset")?;

        let asset_ref = match state.params.backend {
            Some(ref backend) => backend.sync(&atlas_asset, None).await?,
            None => None,
        };

        for packed in &atlas.sprites {
            let Some((path, rel_path, hash)) = sprite_meta.get(&packed.sprite.name) else {
                warn!("No original path found for sprite '{}'", packed.sprite.name);
                continue;
            };

            let event = super::Event::Finished {
                state: super::EventState::Synced { new: true },
                input_name: state.input_name.clone(),
                path: path.clone(),
                rel_path: rel_path.clone(),
                hash: *hash,
                asset_ref: asset_ref.clone(),
                sprite_info: Some(crate::lockfile::SpriteInfo {
                    rect: packed.rect,
                    source_size: packed.sprite.size,
                    trimmed: packed.trimmed,
                    sprite_source_size: packed.sprite_source_size,
                }),
            };
            tx.send(event).unwrap();
        }
    }

    info!(
        "Generated {atlas_count} atlas page(s) for input '{}'",
        state.input_name
    );

    Ok(())
}

/// Check if packing should be enabled for this input
fn should_pack(input: &Input, args: &SyncArgs) -> bool {
    // CLI overrides
    if args.pack {
        return true;
    }
    if args.no_pack {
        return false;
    }

    // Check input configuration
    input.pack.as_ref().is_some_and(|pack| pack.enabled)
}

/// Apply CLI argument overrides to pack options
fn apply_pack_overrides(base_options: Option<&PackOptions>, args: &SyncArgs) -> PackOptions {
    let mut options = base_options.cloned().unwrap_or_default();

    // Apply CLI overrides
    if args.pack {
        options.enabled = true;
    }
    if args.no_pack {
        options.enabled = false;
    }
    if let Some(max_size) = args.pack_max_size {
        options.max_size = max_size;
    }
    if let Some(padding) = args.pack_padding {
        options.padding = padding;
    }
    if let Some(extrude) = args.pack_extrude {
        options.extrude = extrude;
    }
    if let Some(algorithm) = args.pack_algorithm.clone() {
        options.algorithm = algorithm;
    }
    if args.pack_trim {
        options.allow_trim = true;
    }
    if args.pack_no_trim {
        options.allow_trim = false;
    }
    if let Some(page_limit) = args.pack_page_limit {
        options.page_limit = Some(page_limit);
    }
    if let Some(sort) = args.pack_sort.clone() {
        options.sort = sort;
    }
    if args.pack_dedupe {
        options.dedupe = true;
    }

    options
}
