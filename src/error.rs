//! The reconcile error shared by the `CalibanTask` and `Workspace` controllers.

/// Reconcile error.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Kubernetes API error.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),
    /// The task has no `metadata.uid`, so its children can't carry an owner
    /// reference (#49).
    #[error("CalibanTask {0} has no metadata.uid; cannot own its children")]
    MissingUid(String),
}
