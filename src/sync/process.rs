use super::SyncState;
use crate::{asset::Asset, progress_bar::ProgressBar};
use futures::stream::{self, StreamExt};
use log::warn;
use std::sync::Arc;

pub async fn process(
    assets: Vec<Asset>,
    state: Arc<SyncState>,
    input_name: String,
    bleed: bool,
    optimize: bool,
) -> anyhow::Result<Vec<Asset>> {
    let pb = ProgressBar::new(
        state.multi_progress.clone(),
        &format!("Processing input \"{input_name}\""),
        assets.len(),
    );

    let pb = Arc::new(pb);

    let processed_assets: Vec<Asset> = stream::iter(assets)
        .map(|mut asset| {
            let state = state.clone();
            let pb = pb.clone();
            async move {
                let file_name = asset.path.to_string();
                pb.set_msg(&file_name);

                match asset.process(state.font_db.clone(), bleed, optimize).await {
                    Ok(_) => {
                        pb.inc(1);
                        Some(asset)
                    }
                    Err(err) => {
                        warn!("Skipping file {file_name} because it failed processing: {err:?}");
                        pb.inc(1);
                        None
                    }
                }
            }
        })
        .buffer_unordered(num_cpus::get())
        .filter_map(|x| async move { x })
        .collect()
        .await;

    pb.finish();

    Ok(processed_assets)
}
