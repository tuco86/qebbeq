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
    printcolumn = r#"{"name":"Status","type":"string","jsonPath": ".status.phase"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath": ".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"Retain","type":"string","jsonPath": ".status.retain_until", "description": "Time until deletion (if unreferenced)", "priority": 1}"#
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
    /// When the image should be retained until (RFC3339)
    pub retain_until: Option<DateTime<Utc>>,
    /// Last time the image was pulled into the managed registry
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pulled_at: Option<DateTime<Utc>>,
    /// Standard Kubernetes-style conditions (e.g., Ready, ImagePullBackOff)
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// High-level phase summarizing state for kubectl get output (e.g., Ready, ImagePullBackOff, Retaining, Unreferenced, Deleting)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
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
    #[serde(rename = "type")]
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
    upsert_condition(conditions, "Ready", status, reason, message);
}

pub fn upsert_image_pull_backoff_condition(conditions: &mut Vec<Condition>, status: &str, reason: Option<&str>, message: Option<&str>) {
    upsert_condition(conditions, "ImagePullBackOff", status, reason, message);
}

fn upsert_condition(conditions: &mut Vec<Condition>, type_: &str, status: &str, reason: Option<&str>, message: Option<&str>) {
    let now = Utc::now();
    if let Some(c) = conditions.iter_mut().find(|c| c.type_ == type_) {
        if c.status != status {
            c.status = status.to_string();
            c.last_transition_time = Some(now);
        }
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

impl ImageStatus {
    pub fn compute_phase(&self, has_refs: bool, is_deleting: bool) -> String {
        if is_deleting { return "Deleting".to_string(); }
        if has_refs {
            // Look at Ready & ImagePullBackOff
            let ready = self.conditions.iter().find(|c| c.type_ == "Ready");
            let ipb = self.conditions.iter().find(|c| c.type_ == "ImagePullBackOff");
            if let Some(r) = ready {
                match r.status.as_str() {
                    "True" => return "Ready".into(),
                    "False" => {
                        if let Some(i) = ipb { if i.status == "True" { return "ImagePullBackOff".into(); } }
                        return r.reason.clone().unwrap_or_else(|| "Error".into());
                    }
                    _ => return "Reconciling".into(),
                }
            }
            return "Reconciling".into();
        } else {
            if self.retain_until.is_some() { return "Retaining".into(); }
            return "Unreferenced".into();
        }
    }
}
