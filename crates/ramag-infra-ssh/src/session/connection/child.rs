use tokio::process::Child;
use tokio::time::timeout;

use super::CLOSE_TIMEOUT;

pub(super) async fn stop_child(child: &mut Child) {
    if let Err(error) = child.start_kill() {
        tracing::warn!(
            operation = "ssh_sftp_child_stop",
            stage = "kill",
            error = %error,
            "kill ssh sftp child failed"
        );
    }
    if timeout(CLOSE_TIMEOUT, child.wait()).await.is_err() {
        tracing::warn!(
            operation = "ssh_sftp_child_stop",
            stage = "wait",
            "ssh sftp child did not exit after kill"
        );
    }
}
