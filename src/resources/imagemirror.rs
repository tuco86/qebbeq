use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ImageMirror is per upstream repository (not per tag/image). Lifecycle of individual
// images (tags/digests) remains managed by the Image CRs. This resource declares
// how an upstream repository should be accessed and optionally polled for updates.
// Example:
// apiVersion: qebbeq.tuco86.dev/v1alpha1
// kind: ImageMirror
// metadata:
//   name: quay-foo-bar   # Typically derived from upstream (quay-io-<repo-path>)
// spec:
//   upstreamRegistry: quay.io
//   repository: foo/bar
//   policy:
//     type: IfNotPresent
// OR
//   policy:
//     type: Poll
//     intervalSeconds: 300
// status:
//   lastSyncTime: 2025-09-10T12:34:56Z
//   conditions:
//   - type: Ready
//     status: True

#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "qebbeq.tuco86.dev",
    version = "v1alpha1",
    kind = "ImageMirror",
    plural = "imagemirrors",
    namespaced,
    status = "ImageMirrorStatus",
    printcolumn = r#"{"name":"Registry","type":"string","jsonPath": ".spec.upstreamRegistry"}"#,
    printcolumn = r#"{"name":"Repository","type":"string","jsonPath": ".spec.repository"}"#,
    printcolumn = r#"{"name":"Policy","type":"string","jsonPath": ".spec.policy.type"}"#,
    printcolumn = r#"{"name":"Platforms","type":"string","jsonPath": ".spec.platforms"}"#,
    printcolumn = r#"{"name":"LastSync","type":"date","jsonPath": ".status.lastSyncTime"}"#
)]
pub struct ImageMirrorSpec {
    /// Upstream registry hostname (e.g. quay.io or gcr.io)
    #[serde(rename = "upstreamRegistry")]
    pub upstream_registry: String,
    /// Upstream repository path within the registry (e.g. org/name)
    pub repository: String,
    /// Mirroring policy controlling when we attempt to fetch new tags/digests
    pub policy: MirrorPolicy,
    /// Desired platforms to mirror (e.g. ["linux/amd64", "linux/arm64"]). Defaults to ["linux/amd64"].
    #[serde(default = "default_platforms")]
    pub platforms: Vec<String>,
}

fn default_platforms() -> Vec<String> { vec!["linux/amd64".to_string()] }

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MirrorPolicy {
    /// Only mirror when a referenced image (tag/digest) is first needed (lazy fetch).
    IfNotPresent,
    /// Poll upstream for updates (e.g. new tags or changed tag digests) at a fixed interval.
    Poll { #[serde(rename = "intervalSeconds")] interval_seconds: u32 },
}

impl Default for MirrorPolicy {
    fn default() -> Self { MirrorPolicy::IfNotPresent }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ImageMirrorStatus {
    /// Last time a sync (lazy or scheduled) successfully occurred
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_time: Option<DateTime<Utc>>,
    /// Standard conditions (Ready, Syncing, Error, etc.)
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Condition {
    pub type_: String,
    pub status: String, // "True" | "False" | "Unknown"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "lastTransitionTime", skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<DateTime<Utc>>,
}

pub fn upsert_condition(conditions: &mut Vec<Condition>, type_: &str, status: &str, reason: Option<&str>, message: Option<&str>) {
    let now = Utc::now();
    if let Some(c) = conditions.iter_mut().find(|c| c.type_ == type_) {
        if c.status != status { c.status = status.to_string(); c.last_transition_time = Some(now); }
        c.reason = reason.map(|s| s.to_string());
        c.message = message.map(|s| s.to_string());
    } else {
        conditions.push(Condition {
            type_: type_.into(),
            status: status.into(),
            reason: reason.map(|s| s.to_string()),
            message: message.map(|s| s.to_string()),
            last_transition_time: Some(now),
        });
    }
}
