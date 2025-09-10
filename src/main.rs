use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::Utc;
use clap::Parser;
use futures::{StreamExt, TryStreamExt};
use kube::{Api, ResourceExt, Client};
use kube::core::CustomResourceExt;
use kube::api::{Patch, PatchParams};
use kube::runtime::{watcher, watcher::Config};
mod resources;
use resources::image::{Image, upsert_ready_condition, ObjectKey};
use tokio::sync::mpsc;
use k8s_openapi::api::core::v1::Pod;
use tokio::task::JoinSet;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use sha2::{Sha256, Digest};
use std::fs;


// main moved to bottom so helper fns are in scope

async fn reconcile_image(images: Api<Image>, image: Image, provided_refs: &BTreeSet<ObjectKey>, retention: Duration) -> anyhow::Result<()> {
    // Ensure our finalizer is present early
    const FINALIZER: &str = "qebbeq.tuco86.dev/finalizer";
    let is_deleting = image.metadata.deletion_timestamp.is_some();
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

    // Sync references to provided set
    let mut status = image.status.clone().unwrap_or_default();
    status.references = provided_refs.clone();
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

    // If deleting and no refs -> remove finalizer immediately; skip retention
    if is_deleting && !has_refs {
        let name = image.name_any();
        let new_fins: Vec<String> = image
            .metadata
            .finalizers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f != FINALIZER)
            .collect();
        let patch = serde_json::json!({ "metadata": { "finalizers": new_fins } });
        let _ = images
            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
    }

    // If not deleting and no refs -> schedule delayed finalizer removal based on retention
    if !is_deleting && !has_refs {
        let last = status.last_unreferenced.unwrap_or_else(Utc::now);
        let elapsed = (Utc::now() - last).to_std().unwrap_or_default();
        let remaining = if elapsed >= retention { Duration::ZERO } else { retention - elapsed };
        let name = image.name_any();

        // Spawn a delayed task that re-checks and removes finalizer if still unreferenced
        let images_clone = images.clone();
        let refs_snapshot = provided_refs.clone();
        tokio::spawn(async move {
            if !remaining.is_zero() { tokio::time::sleep(remaining).await; }
            if let Ok(current) = images_clone.get(&name).await {
                let still_zero = refs_snapshot.is_empty() && current.status.as_ref().map(|s| s.references.is_empty()).unwrap_or(true);
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

// ---------------- Types for reference updates & watchers ----------------

#[derive(Debug, Clone)]
struct ResourceImageRef { #[allow(dead_code)] path: String, image: String }

#[derive(Debug, Clone)]
enum ReferenceUpdate {
    Upsert { resource: ObjectKey, images: Vec<ResourceImageRef> },
    Delete(ObjectKey),
}

fn pod_object_key(pod: &Pod) -> ObjectKey {
    ObjectKey {
        api_version: "v1".into(),
        kind: "Pod".into(),
        namespace: pod.namespace().unwrap_or_default(),
        name: pod.name_any(),
    }
}

fn collect_pod_image_refs(pod: &Pod) -> Vec<ResourceImageRef> {
    let mut out = Vec::new();
    if let Some(spec) = &pod.spec {
        for (i, c) in spec.containers.iter().enumerate() {
            if let Some(img) = &c.image { out.push(ResourceImageRef { path: format!(".spec.containers[{i}].image"), image: img.clone() }); }
        }
        if let Some(inits) = &spec.init_containers {
            for (i, c) in inits.iter().enumerate() {
                if let Some(img) = &c.image { out.push(ResourceImageRef { path: format!(".spec.initContainers[{i}].image"), image: img.clone() }); }
            }
        }
        if let Some(eps) = &spec.ephemeral_containers {
            for (i, c) in eps.iter().enumerate() {
                if let Some(img) = &c.image { out.push(ResourceImageRef { path: format!(".spec.ephemeralContainers[{i}].image"), image: img.clone() }); }
            }
        }
    }
    out
}

async fn run_pod_watcher(token: CancellationToken, client: Client, tx: mpsc::Sender<ReferenceUpdate>) -> anyhow::Result<()> {
    let pods: Api<Pod> = Api::all(client);
    let mut stream = watcher(pods.clone(), Config::default()).boxed();
    loop {
        tokio::select! {
            _ = token.cancelled() => { break; }
            ev = stream.try_next() => {
                match ev {
                    Ok(Some(event)) => {
                        match event {
                            watcher::Event::Apply(p) | watcher::Event::InitApply(p) => {
                                let key = pod_object_key(&p);
                                let images = collect_pod_image_refs(&p);
                                let _ = tx.send(ReferenceUpdate::Upsert { resource: key, images }).await;
                            }
                            watcher::Event::Delete(p) => {
                                let key = pod_object_key(&p);
                                let _ = tx.send(ReferenceUpdate::Delete(key)).await;
                            }
                            watcher::Event::Init | watcher::Event::InitDone => {}
                        }
                    }
                    Ok(None) => break,
                    Err(e) => { tracing::error!(error=?e, "pod watcher stream error"); }
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct ControllerConfig {
    namespace: String,
    registry_host: String,
}

async fn run_image_watcher(token: CancellationToken, images_api: Api<Image>, client: Client, mut rx: mpsc::Receiver<ReferenceUpdate>, retention: Duration, cfg: ControllerConfig) -> anyhow::Result<()> {
    // Indices
    let mut by_resource: BTreeMap<ObjectKey, BTreeSet<String>> = BTreeMap::new();
    let mut by_image: BTreeMap<String, BTreeSet<ObjectKey>> = BTreeMap::new();

    // Image CR events stream
    let mut image_events = watcher(images_api.clone(), Config::default()).boxed();

    loop {
        tokio::select! {
            _ = token.cancelled() => { break; }
            maybe_ev = image_events.try_next() => {
                match maybe_ev {
                    Ok(Some(ev)) => {
                        match ev {
                            watcher::Event::Apply(img) | watcher::Event::InitApply(img) => {
                                let img_ref = img.spec.image.clone();
                                let refs = by_image.get(&img_ref).cloned().unwrap_or_default();
                                let _ = reconcile_image(images_api.clone(), img, &refs, retention).await;
                            }
                            watcher::Event::Delete(_img) => {}
                            watcher::Event::Init | watcher::Event::InitDone => {}
                        }
                    }
                    Ok(None) => break,
                    Err(e) => { tracing::error!(error=?e, "image watcher stream error"); }
                }
            }
            Some(update) = rx.recv() => {
                match update {
                    ReferenceUpdate::Upsert { resource, images } => {
                        // Filter to only images belonging to configured registry_host
                        let new_set: BTreeSet<String> = images.iter()
                            .map(|r| r.image.clone())
                            .filter(|img| image_matches_host(img, &cfg.registry_host))
                            .collect();
                        let old_set = by_resource.get(&resource).cloned().unwrap_or_default();
                        let affected: BTreeSet<String> = old_set.union(&new_set).cloned().collect();

                        // Create Image CRs for any newly seen image refs (namespaced to resource namespace)
                        for img_ref in new_set.iter() {
                            if !by_image.contains_key(img_ref) {
                                if let Some(created_name) = ensure_image_cr(&client, &cfg.namespace, img_ref).await? {
                                    tracing::info!(image_ref=%img_ref, cr_name=%created_name, ns=%cfg.namespace, "created Image CR");
                                    // No immediate reconcile here; will happen below after indices updated so refs & Ready are accurate.
                                }
                            }
                        }

                        // Remove old refs
                        for img in old_set.difference(&new_set) {
                            if let Some(set) = by_image.get_mut(img) { set.remove(&resource); if set.is_empty() { by_image.remove(img); } }
                        }
                        // Add new refs
                        for img in new_set.iter() { by_image.entry(img.clone()).or_default().insert(resource.clone()); }
                        if new_set.is_empty() { by_resource.remove(&resource); } else { by_resource.insert(resource.clone(), new_set.clone()); }

                        // Reconcile affected Image CRs
                        for image_ref in affected.into_iter() {
                            if let Some(image_objs) = find_image_crs(&images_api, &image_ref).await? {
                                for img in image_objs {
                                    let refs = by_image.get(&image_ref).cloned().unwrap_or_default();
                                    let _ = reconcile_image(images_api.clone(), img, &refs, retention).await;
                                }
                            }
                        }
                    }
                    ReferenceUpdate::Delete(resource) => {
                        if let Some(old_set) = by_resource.remove(&resource) {
                            for img in old_set.iter() {
                                if let Some(set) = by_image.get_mut(img) { set.remove(&resource); if set.is_empty() { by_image.remove(img); } }
                            }
                            // Reconcile images impacted
                            for img_ref in old_set.into_iter() {
                                if let Some(image_objs) = find_image_crs(&images_api, &img_ref).await? {
                                    for img in image_objs {
                                        let refs = by_image.get(&img_ref).cloned().unwrap_or_default();
                                        let _ = reconcile_image(images_api.clone(), img, &refs, retention).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

async fn find_image_crs(images_api: &Api<Image>, image_ref: &str) -> anyhow::Result<Option<Vec<Image>>> {
    // NOTE: naive list & filter; optimize later (informers / indexers / label mapping)
    let list = images_api.list(&Default::default()).await?;
    let matches: Vec<Image> = list.into_iter().filter(|i| i.spec.image == image_ref).collect();
    if matches.is_empty() { Ok(None) } else { Ok(Some(matches)) }
}

async fn ensure_image_cr(client: &Client, namespace: &str, image_ref: &str) -> anyhow::Result<Option<String>> {
    // List existing to avoid duplicates (could optimize with direct get if we had deterministic name)
    let candidate_name = generate_image_name(image_ref);
    let images_ns: Api<Image> = Api::namespaced(client.clone(), namespace);
    if images_ns.get_opt(&candidate_name).await?.is_some() {
        return Ok(None);
    }
    let img = Image::new(&candidate_name, resources::image::ImageSpec { image: image_ref.to_string() });
    match images_ns.create(&Default::default(), &img).await {
        Ok(_) => Ok(Some(candidate_name)),
        Err(e) => {
            tracing::warn!(error=?e, image_ref=%image_ref, "failed to create Image CR");
            Ok(None)
        }
    }
}

fn generate_image_name(image_ref: &str) -> String {
    // Hash full ref for compact DNS-safe name; prefix with "img-"; truncate hash for readability
    let mut hasher = Sha256::new();
    hasher.update(image_ref.as_bytes());
    let hash = hex::encode(hasher.finalize());
    // Keep first 16 chars
    let short = &hash[..16];
    let base_part = image_ref
        .rsplit('/')
        .next()
        .unwrap_or("img")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.', "-")
        .to_lowercase();
    let trimmed = if base_part.len() > 30 { &base_part[..30] } else { &base_part };
    format!("img-{trimmed}-{short}")
}

fn image_matches_host(image_ref: &str, host: &str) -> bool {
    match extract_image_host(image_ref) {
        Some(h) => h == host,
        None => false, // unqualified images ignored; could optionally treat as local
    }
}

fn extract_image_host(image_ref: &str) -> Option<&str> {
    let first = image_ref.split('/').next().unwrap_or("");
    if first.contains('.') || first.contains(':') || first == "localhost" { Some(first) } else { None }
}

fn detect_controller_namespace(explicit: &Option<String>) -> String {
    if let Some(ns) = explicit { return ns.clone(); }
    if let Ok(c) = fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace") {
        let trimmed = c.trim();
        if !trimmed.is_empty() { return trimmed.to_string(); }
    }
    std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.print_crd {
        let crd = Image::crd();
        println!("{}", serde_yaml::to_string(&crd)?);
        return Ok(());
    }

    // Initialize tracing (ignore error if already set)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let token = CancellationToken::new();
    let mut set: JoinSet<anyhow::Result<()>> = JoinSet::new();
    let client = Client::try_default().await?;
    let images: Api<Image> = Api::all(client.clone());
    let controller_namespace = detect_controller_namespace(&args.controller_namespace);
    let cfg = ControllerConfig { namespace: controller_namespace, registry_host: args.registry_host.clone() };
    let (tx, rx) = mpsc::channel::<ReferenceUpdate>(512);

    // Pod watcher
    {
        let t = token.clone();
        let client_pods = client.clone();
        let tx_pods = tx.clone();
        set.spawn(async move { run_pod_watcher(t, client_pods, tx_pods).await });
    }

    // Image watcher
    {
        let t = token.clone();
        let images_api = images.clone();
        let client_clone = client.clone();
    let cfg_clone = cfg.clone();
    set.spawn(async move { run_image_watcher(t, images_api, client_clone, rx, args.retention, cfg_clone).await });
    }

    let result: anyhow::Result<()> = tokio::select! {
        _ = signal::ctrl_c() => {
            token.cancel();
            while let Some(_res) = set.join_next().await {}
            eprintln!("shutting down cleanly (Ctrl-C)");
            Ok(())
        }
        join_out = set.join_next() => {
            match join_out { // Option<Result<anyhow::Result<()>, JoinError>>
                None => Ok(()), // no tasks? just exit
                Some(Err(join_err)) => {
                    let e = anyhow::Error::from(join_err);
                    eprintln!("watcher join failed: {e:#}");
                    Err(e)
                }
                Some(Ok(task_result)) => {
                    match task_result {
                        Ok(()) => {
                            eprintln!("a watcher exited unexpectedly without error; shutting down");
                            Ok(())
                        }
                        Err(e) => {
                            eprintln!("watcher failed: {e:#}");
                            Err(e)
                        }
                    }
                }
            }
        }
    };
    token.cancel();
    while let Some(_res) = set.join_next().await {}
    result
}

#[derive(Parser, Debug, Clone)]
#[command(name = "qebbeq", version, about = "Image reference controller", long_about = None)]
struct Args {
    /// Retention duration after last unreference (e.g., 60s, 5m, 1h, 2d)
    #[arg(long, value_parser = humantime::parse_duration, default_value = "1h")]
    retention: Duration,
    /// Print the CRD YAML to stdout and exit
    #[arg(long = "print-crd")]
    print_crd: bool,
    /// Namespace where controller runs (defaults to serviceaccount namespace or 'default')
    #[arg(long, env = "QEBBEQ_NAMESPACE")]
    controller_namespace: Option<String>,
    /// Registry hostname this controller manages (only images from this host are tracked)
    #[arg(long, env = "QEBBEQ_REGISTRY_HOST")]
    registry_host: String,
}
