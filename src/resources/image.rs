use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "qebbeq.tuco86.dev",
    version = "v1alpha1",
    kind = "Image",
    plural = "images",
    namespaced,
    status = "ImageStatus",
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"LastUnref","type":"date","jsonPath":".status.last_unreferenced"}"#
)]
pub struct ImageSpec {
    /// Full image reference (e.g., registry/repo@sha256:... or registry/repo:tag)
    pub image: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ImageStatus {
    /// The current set of object references that use this image
    #[serde(default)]
    pub references: BTreeSet<ObjectKey>,
    /// When the last reference was removed (RFC3339)
    pub last_unreferenced: Option<DateTime<Utc>>,
    /// Whether the image blob has been mirrored/pulled into the managed registry (None = unknown/not attempted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirrored: Option<bool>,
    /// Last time a mirror (pull/push) completed successfully
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_mirror_time: Option<DateTime<Utc>>,
    /// Standard Kubernetes-style conditions (e.g., Ready)
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Deserialize, Serialize, JsonSchema, Default)]
pub struct ObjectKey {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
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

pub fn upsert_ready_condition(conditions: &mut Vec<Condition>, status: &str, reason: Option<&str>, message: Option<&str>) {
    let now = Utc::now();
    if let Some(c) = conditions.iter_mut().find(|c| c.type_ == "Ready") {
        if c.status != status {
            c.status = status.to_string();
            c.last_transition_time = Some(now);
        }
        c.reason = reason.map(|s| s.to_string());
        c.message = message.map(|s| s.to_string());
    } else {
        conditions.push(Condition {
            type_: "Ready".into(),
            status: status.into(),
            reason: reason.map(|s| s.to_string()),
            message: message.map(|s| s.to_string()),
            last_transition_time: Some(now),
        });
    }
}
