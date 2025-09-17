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
use resources::imagemirror::{ImageMirror, MirrorPolicyType, upsert_condition as upsert_mirror_condition};
use tokio::sync::mpsc;
use k8s_openapi::api::core::v1::Pod;
use tokio::task::JoinSet;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use sha2::{Sha256, Digest};
use std::fs;
use tokio::process::Command;


// main moved to bottom so helper fns are in scope

async fn reconcile_image(client: Client, image: Image, provided_refs: &BTreeSet<ObjectKey>, retention: Duration) -> anyhow::Result<()> {
    let _ns = image.namespace().unwrap_or_default();
    let images: Api<Image> = Api::namespaced(client.clone(), &_ns);
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
        if let Err(e) = images
            .patch(&image.name_any(), &PatchParams::apply("qebbeq").force(), &Patch::Merge(&patch))
            .await {
            tracing::warn!(error=?e, name=%image.name_any(), "failed to add finalizer");
        }
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
    // Mirroring integration: if referenced and not mirrored, attempt mirror via crane
    if has_refs && status.mirrored != Some(true) {
        if let Err(e) = mirror_with_crane_for_image(&client, &image).await {
            tracing::warn!(error=?e, image=%image.name_any(), "crane mirror failed (leaving mirrored unset)");
        } else {
            status.mirrored = Some(true);
            status.last_mirror_time = Some(Utc::now());
        }
    }
    // Ready semantics: referenced & mirrored => True else Unknown
    let ready_status = if has_refs && status.mirrored == Some(true) { "True" } else { "Unknown" };
    upsert_ready_condition(&mut status.conditions, ready_status, Some("Reconciled"), None);

    // Patch status if changed
    let patch_needed = image.status.as_ref() != Some(&status);
    if patch_needed {
        let ps = serde_json::json!({ "status": status });
        if let Err(e) = images
            .patch_status(&image.name_any(), &PatchParams::default(), &Patch::Merge(&ps))
            .await {
            tracing::warn!(error=?e, name=%image.name_any(), "patch_status failed; attempting full merge patch");
            let full = serde_json::json!({"status": ps["status"]});
            if let Err(e2) = images.patch(&image.name_any(), &PatchParams::default(), &Patch::Merge(&full)).await {
                tracing::error!(error=?e2, name=%image.name_any(), "fallback status merge failed");
            }
        }
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
    if let Err(e) = images
            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await {
            tracing::warn!(error=?e, name=%name, "failed to remove finalizer immediately");
        }
    }

    // If not deleting and no refs -> schedule delayed finalizer removal based on retention
    if !is_deleting && !has_refs {
        let last = status.last_unreferenced.unwrap_or_else(Utc::now);
        let elapsed = (Utc::now() - last).to_std().unwrap_or_default();
        let remaining = if elapsed >= retention { Duration::ZERO } else { retention - elapsed };
        let name = image.name_any();

        // Spawn a delayed task that re-checks and removes finalizer if still unreferenced
    let client_clone = client.clone();
    let ns_clone = _ns.clone();
        let refs_snapshot = provided_refs.clone();
        tokio::spawn(async move {
            if !remaining.is_zero() { tokio::time::sleep(remaining).await; }
            let images_ns: Api<Image> = Api::namespaced(client_clone.clone(), &ns_clone);
            if let Ok(current) = images_ns.get(&name).await {
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
                    if let Err(e) = images_ns.patch(&name, &PatchParams::default(), &Patch::Merge(&patch)).await {
                        tracing::warn!(error=?e, name=%name, "failed to remove finalizer after retention");
                    }
                }
            }
        });
    }

    Ok(())
}

// Crane integration: best-effort copy for the referenced image based on matching ImageMirror
async fn mirror_with_crane_for_image(client: &Client, image: &Image) -> anyhow::Result<()> {
    // Determine upstream by looking up ImageMirror in controller namespace
    let _ns = image.namespace().unwrap_or_default();
    // We aggregate Image CRs in controller namespace; ImageMirror is namespaced, we look cluster-wide then prefer same ns
    let mirrors: Api<ImageMirror> = Api::all(client.clone());
    let src_ref = &image.spec.image; // expected local ref
    // Extract repo path after hostname, up to tag/digest
    let local_repo = {
        let bytes = src_ref.as_bytes();
    // find first '/'
    let i = if let Some(slash) = src_ref.find('/') { slash + 1 } else { 0 };
        let mut j = bytes.len();
        if let Some(at) = src_ref[i..].find('@') { j = i + at; }
        if let Some(col) = src_ref[i..].rfind(':') {
            // only treat ':' as tag if it is after last '/'
            let abs = i + col;
            if let Some(last_slash) = src_ref.rfind('/') { if abs > last_slash { j = j.min(abs); } }
        }
        &src_ref[i..j]
    };
    let list = mirrors.list(&Default::default()).await?;
    // Find first mirror whose repository suffix matches local_repo (simple heuristic)
    // Prefer exact match on repository suffix; if multiple, prefer ones in controller namespace
    let mut candidates: Vec<ImageMirror> = list.into_iter().filter(|m| m.spec.repository.ends_with(local_repo)).collect();
    // Stable sort: same-namespace first (if Image and Mirror share ns); then by name
    let image_ns = image.namespace();
    candidates.sort_by_key(|m| (if Some(m.namespace().unwrap_or_default()) == image_ns { 0 } else { 1 }, m.name_any()));
    let candidate = candidates.into_iter().next();
    let Some(mirror) = candidate else {
        anyhow::bail!("no ImageMirror found matching repo {local_repo}");
    };

    // Build upstream source ref: <upstreamRegistry>/<repository> plus tag/digest from local spec.image
    let (tag_or_digest, _is_digest) = extract_tag_or_digest(src_ref);
    let upstream = format!("{}/{}{}", mirror.spec.upstream_registry, mirror.spec.repository, tag_or_digest.unwrap_or_default());
    // Destination is the local image spec.image as-is
    let dest = src_ref.clone();
    // For each requested platform perform a multi-platform copy. If multiple platforms are specified, run sequential copies.
    if mirror.spec.platforms.len() > 1 {
        tracing::warn!(platforms=?mirror.spec.platforms, "multiple platforms specified; using first only for initial implementation");
    }
    let plat = mirror.spec.platforms.get(0).map(|s| s.as_str());
    crane_copy(&upstream, &dest, plat).await?;
    Ok(())
}

fn extract_tag_or_digest(image_ref: &str) -> (Option<String>, bool) {
    if let Some(idx) = image_ref.find('@') { return (Some(image_ref[idx..].to_string()), true); }
    if let Some(idx) = image_ref.rfind(':') {
        // ensure ':' is after last '/'
        if let Some(slash) = image_ref.rfind('/') { if idx > slash { return (Some(image_ref[idx..].to_string()), false); } }
    }
    (None, false)
}

async fn crane_copy(src: &str, dst: &str, platform: Option<&str>) -> anyhow::Result<()> {
    let mut cmd = Command::new("crane");
    cmd.arg("copy");
    if let Some(p) = platform { cmd.arg("--platform").arg(p); }
    cmd.arg(src).arg(dst);
    tracing::info!(src=%src, dst=%dst, platform=?platform, "running crane copy");
    let output = cmd.output().await.map_err(|e| anyhow::anyhow!("failed to spawn 'crane': {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        tracing::error!(code=?output.status.code(), %stderr, %stdout, "crane copy failed");
        anyhow::bail!("crane copy failed");
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

// ---------------- ImageMirror watcher & polling scheduler (Step 1) ----------------

use std::collections::HashMap;
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct MirrorKey { ns: String, name: String }

async fn patch_imagemirror_status(api: &Api<ImageMirror>, mirror: &ImageMirror, last_sync: Option<chrono::DateTime<chrono::Utc>>, ready: &str, reason: Option<&str>, message: Option<&str>) {
    let mut status = mirror.status.clone().unwrap_or_default();
    if let Some(ts) = last_sync { status.last_sync_time = Some(ts); }
    upsert_mirror_condition(&mut status.conditions, "Ready", ready, reason, message);
    let ps = serde_json::json!({"status": status});
    if let Err(e) = api.patch_status(&mirror.name_any(), &PatchParams::default(), &Patch::Merge(&ps)).await {
        tracing::warn!(error=?e, mirror=%mirror.name_any(), "failed to patch ImageMirror status");
    }
}

async fn run_imagemirror_watcher(token: CancellationToken, client: Client) -> anyhow::Result<()> {
    let api: Api<ImageMirror> = Api::all(client.clone());
    let mut stream = watcher(api.clone(), Config::default()).boxed();
    // map of active poll tasks keyed by mirror key
    let mut poll_tasks: HashMap<MirrorKey, CancellationToken> = HashMap::new();

    loop {
        tokio::select! {
            _ = token.cancelled() => { break; }
            ev = stream.try_next() => {
                match ev {
                    Ok(Some(event)) => match event {
                        watcher::Event::Apply(m) | watcher::Event::InitApply(m) => {
                            let key = MirrorKey { ns: m.namespace().unwrap_or_default(), name: m.name_any() };
                            // Cancel existing poller if any (will recreate below if still Poll)
                            if let Some(cancel) = poll_tasks.remove(&key) { cancel.cancel(); }
                            match m.spec.policy.type_ {
                                MirrorPolicyType::IfNotPresent => {
                                    patch_imagemirror_status(&api, &m, None, "True", Some("Accepted"), Some("Lazy fetch (IfNotPresent)" )).await;
                                }
                                MirrorPolicyType::Poll => {
                                    patch_imagemirror_status(&api, &m, None, "True", Some("Polling"), Some("Active polling" )).await;
                                    let child_cancel = CancellationToken::new();
                                    let child_cancel_clone = child_cancel.clone();
                                    let api_clone = api.clone();
                                    let name = m.name_any();
                                    let ns = key.ns.clone();
                                    let secs = m.spec.policy.interval_seconds.unwrap_or(300);
                                    let interval = Duration::from_secs(secs.into());
                                    let outer_token = token.clone();
                                    tokio::spawn(async move {
                                        let mirrors_ns: Api<ImageMirror> = Api::namespaced(api_clone.clone().into_client(), &ns);
                                        loop {
                                            tokio::select! {
                                                _ = child_cancel_clone.cancelled() => break,
                                                _ = outer_token.cancelled() => break,
                                                _ = tokio::time::sleep(interval) => {
                                                    // Fetch current to ensure it still exists & policy is Poll
                                                    match mirrors_ns.get_opt(&name).await {
                                                        Ok(Some(cur)) => {
                                                            if cur.spec.policy.type_ == MirrorPolicyType::Poll {
                                                                // Placeholder trigger; real mirroring in Step 2
                                                                tracing::info!(mirror=%name, namespace=%ns, "poll tick (placeholder)");
                                                                // Update last_sync_time to show activity
                                                                let now = Utc::now();
                                                                let mut status = cur.status.clone().unwrap_or_default();
                                                                status.last_sync_time = Some(now);
                                                                upsert_mirror_condition(&mut status.conditions, "Ready", "True", Some("Polling"), Some("Poll tick"));
                                                                let ps = serde_json::json!({"status": status});
                                                                if let Err(e) = mirrors_ns.patch_status(&name, &PatchParams::default(), &Patch::Merge(&ps)).await {
                                                                    tracing::warn!(error=?e, mirror=%name, "failed to patch poll tick status");
                                                                }
                                                            } else { break; }
                                                        }
                                                        Ok(None) => break, // mirror deleted
                                                        Err(e) => {
                                                            tracing::warn!(error=?e, mirror=%name, "error fetching mirror during poll");
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        tracing::debug!(mirror=%name, namespace=%ns, "poll task exiting");
                                    });
                                    poll_tasks.insert(key, child_cancel);
                                }
                            }
                        }
                        watcher::Event::Delete(m) => {
                            let key = MirrorKey { ns: m.namespace().unwrap_or_default(), name: m.name_any() };
                            if let Some(cancel) = poll_tasks.remove(&key) { cancel.cancel(); }
                        }
                        watcher::Event::Init | watcher::Event::InitDone => {}
                    },
                    Ok(None) => break,
                    Err(e) => { tracing::error!(error=?e, "imagemirror watcher stream error"); }
                }
            }
        }
    }
    // Cancel any remaining pollers
    for (_, c) in poll_tasks.into_iter() { c.cancel(); }
    Ok(())
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
                                let _ = reconcile_image(client.clone(), img, &refs, retention).await;
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
                                    let _ = reconcile_image(client.clone(), img, &refs, retention).await;
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
                                        let _ = reconcile_image(client.clone(), img, &refs, retention).await;
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
        let mut docs = Vec::new();
        docs.push(serde_yaml::to_value(Image::crd())?);
        docs.push(serde_yaml::to_value(ImageMirror::crd())?);
        // Print as multi-document YAML
        for (i, d) in docs.iter().enumerate() { if i>0 { println!("---"); } println!("{}", serde_yaml::to_string(d)?); }
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

    // ImageMirror watcher (Step 1 skeleton)
    {
        let t = token.clone();
        let client_clone = client.clone();
        set.spawn(async move { run_imagemirror_watcher(t, client_clone).await });
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
