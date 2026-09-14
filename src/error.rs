//! The reconcile error shared by the `CalibanTask` and `Workspace` controllers.

/// Reconcile error.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Kubernetes API error.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),
}
