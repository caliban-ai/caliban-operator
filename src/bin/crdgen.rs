//! Emit a CRD's YAML: `cargo run --bin crdgen <calibantask|workspace> > deploy/crd/<kind>.yaml`.

use kube::CustomResourceExt;

fn main() -> anyhow::Result<()> {
    let kind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "calibantask".into());
    let yaml = match kind.as_str() {
        "calibantask" => serde_norway::to_string(&caliban_operator::crd::CalibanTask::crd())?,
        "workspace" => serde_norway::to_string(&caliban_operator::workspace::Workspace::crd())?,
        other => anyhow::bail!("unknown CRD kind {other:?}; expected calibantask|workspace"),
    };
    print!("{yaml}");
    Ok(())
}
