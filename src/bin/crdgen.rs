//! Emit the CalibanTask CRD YAML: `cargo run --bin crdgen > deploy/crd/calibantask.yaml`.

use kube::CustomResourceExt;

fn main() -> anyhow::Result<()> {
    let crd = caliban_operator::crd::CalibanTask::crd();
    print!("{}", serde_yaml::to_string(&crd)?);
    Ok(())
}
