//! caliban-operator — a kube-rs operator that reconciles `CalibanTask` custom
//! resources into sandboxed caliband pods (via agent-sandbox). See ADR 0001 and
//! the k8s system-design spec (epic caliban-ai/caliban#274).

pub mod config;
pub mod controller;
pub mod crd;
pub mod resources;
pub mod sandbox;

pub use crd::{CalibanTask, CalibanTaskSpec, CalibanTaskStatus, Phase};
