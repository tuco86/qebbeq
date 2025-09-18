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
    printcolumn = r#"{"name":"Upstream","type":"string","jsonPath": ".spec.upstream"}"#,
    printcolumn = r#"{"name":"Prefix","type":"string","jsonPath": ".spec.prefix"}"#,
    printcolumn = r#"{"name":"Policy","type":"string","jsonPath": ".spec.policy.type"}"#,
    printcolumn = r#"{"name":"Platforms","type":"string","jsonPath": ".spec.platforms"}"#
)]
pub struct ImageMirrorSpec {
    /// Upstream registry or full prefix (e.g. quay.io, gcr.io, or gcr.io/org)
    pub upstream: String,
    /// Prefix to match in local image refs (e.g. oci.mydomain.com/foo/bar). Longest prefix wins.
    pub prefix: String,
    /// Mirroring policy controlling when we attempt to fetch new tags/digests
    pub policy: MirrorPolicy,
    /// Desired platforms to mirror (e.g. ["linux/amd64", "linux/arm64"]). Defaults to ["linux/amd64"].
    #[serde(default = "default_platforms")]
    pub platforms: Vec<String>,
}

fn default_platforms() -> Vec<String> { vec!["linux/amd64".to_string()] }

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MirrorPolicy {
    /// Policy type: IfNotPresent | Poll
    #[serde(rename = "type")]
    pub type_: MirrorPolicyType,
    /// Polling interval in seconds (used when type==Poll)
    #[serde(rename = "intervalSeconds", skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u32>,
}

impl Default for MirrorPolicy {
    fn default() -> Self { Self { type_: MirrorPolicyType::IfNotPresent, interval_seconds: None } }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum MirrorPolicyType { IfNotPresent, Poll }
