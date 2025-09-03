use std::time::Duration;

use chrono::Utc;
use clap::Parser;
use futures::TryStreamExt;
use kube::{Api, ResourceExt, Client};
use kube::core::CustomResourceExt;
use kube::api::{Patch, PatchParams};
use kube::runtime::{watcher, watcher::Config};
mod resources;
use resources::image::{Image, upsert_ready_condition};


#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // Utility: print the CRD manifest and exit
    if std::env::args().any(|a| a == "--print-crd") {
        let crd = Image::crd();
        println!("{}", serde_yaml::to_string(&crd)?);
        return Ok(());
    }

    let client = Client::try_default().await?;
    let images: Api<Image> = Api::all(client.clone());

    // Watch Image CRs and reconcile on each apply/delete
    watcher(images.clone(), Config::default())
        .try_for_each(|event| {
            let images = images.clone();
            let retention = args.retention;
            async move {
                match event {
                    watcher::Event::Apply(img) => {
                        let _ = reconcile_image(images.clone(), img, retention).await;
                    }
                    watcher::Event::InitApply(img) => {
                        let _ = reconcile_image(images.clone(), img, retention).await;
                    }
                    watcher::Event::Delete(_img) => {
                        // nothing
                    }
                    watcher::Event::Init | watcher::Event::InitDone => {}
                }
                Ok(())
            }
        })
        .await?;

    Ok(())
}

async fn reconcile_image(images: Api<Image>, image: Image, retention: Duration) -> anyhow::Result<()> {
    // Ensure our finalizer is present early
    const FINALIZER: &str = "qebbeq.tuco86.dev/finalizer";
    if !image
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|x| x == FINALIZER))
        .unwrap_or(false)
    {
        let mut fins = image.metadata.finalizers.clone().unwrap_or_default();
        fins.push(FINALIZER.to_string());
        let patch = serde_json::json!({ "metadata": { "finalizers": fins } });
        let _ = images
            .patch(&image.name_any(), &PatchParams::apply("qebbeq").force(), &Patch::Merge(&patch))
            .await?;
    }

    // Compute last_unreferenced from status.references
    let mut status = image.status.clone().unwrap_or_default();
    let has_refs = !status.references.is_empty();
    if !has_refs {
        if status.last_unreferenced.is_none() {
            status.last_unreferenced = Some(Utc::now());
        }
    } else {
        status.last_unreferenced = None;
    }
    // For now, mark Ready as True when referenced, Unknown otherwise (placeholder for registry availability)
    upsert_ready_condition(&mut status.conditions, if has_refs { "True" } else { "Unknown" }, None, None);

    // Patch status if changed
    let patch_needed = image.status.as_ref() != Some(&status);
    if patch_needed {
        let ps = serde_json::json!({ "status": status });
        let _ = images
            .patch_status(&image.name_any(), &PatchParams::default(), &Patch::Merge(&ps))
            .await?;
    }

    // If no refs: schedule delayed finalizer removal
    if !has_refs {
        let last = status.last_unreferenced.unwrap_or_else(Utc::now);
        let elapsed = (Utc::now() - last).to_std().unwrap_or_default();
        let remaining = if elapsed >= retention { Duration::ZERO } else { retention - elapsed };
        let name = image.name_any();

        // Spawn a delayed task that re-checks and removes finalizer if still unreferenced
        let images_clone = images.clone();
        tokio::spawn(async move {
            if !remaining.is_zero() { tokio::time::sleep(remaining).await; }
            if let Ok(current) = images_clone.get(&name).await {
                let still_zero = current.status.as_ref().map(|s| s.references.is_empty()).unwrap_or(true);
                let has_finalizer = current
                    .metadata
                    .finalizers
                    .as_ref()
                    .map(|f| f.iter().any(|x| x == FINALIZER))
                    .unwrap_or(false);
                if still_zero && has_finalizer {
                    // TODO: delete from backing registry here
                    let new_fins: Vec<String> = current
                        .metadata
                        .finalizers
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|f| f != FINALIZER)
                        .collect();
                    let patch = serde_json::json!({ "metadata": { "finalizers": new_fins } });
                    let _ = images_clone
                        .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                        .await;
                }
            }
        });
    }

    Ok(())
}

#[derive(Parser, Debug, Clone)]
#[command(name = "qebbeq", version, about = "Image reference controller", long_about = None)]
struct Args {
    /// Retention duration after last unreference (e.g., 60s, 5m, 1h, 2d)
    #[arg(long, value_parser = humantime::parse_duration, default_value = "1h")]
    retention: Duration,
}
