use tokio::process::{Child, Command};

/// call the kubectl port-forward command.
///
/// We don't use kube crate because calling the library is too tedious
/// and we need to close the portforwarder manually.
/// Within this command approach, we can use the `kill_on_drop` method
/// which is way more convenient to close the portforwarder automatically.
pub async fn port_forward(
    kubectl: &str,
    ns: &str,
    target: &str,
    remote_port: u16,
    local_port: u16,
) -> anyhow::Result<Child> {
    let mut command = Command::new(kubectl);
    let child = command
        .kill_on_drop(true)
        .args([
            "port-forward",
            "-n",
            ns,
            target,
            &format!("{}:{}", local_port, remote_port),
        ])
        .spawn()?;
    Ok(child)
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_not_work() {
        let result = port_forward("not_found", "gateway", "gateway-istio", 8080, 8080).await;
        assert!(result.is_err());
    }
}
