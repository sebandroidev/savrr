//! Best-effort desktop notifications (PRD-07 §3). Toasts are a nicety, never a
//! dependency of correctness — every failure is swallowed to a debug log so a
//! headless box or a locked session never breaks the sync path.

/// Show a toast. Runs the (potentially blocking) platform call on a blocking
/// thread so it can be awaited from the async runtime without stalling it.
pub async fn toast(title: impl Into<String>, body: impl Into<String>) {
    let title = title.into();
    let body = body.into();
    let _ = tokio::task::spawn_blocking(move || toast_blocking(&title, &body)).await;
}

/// Synchronous toast for non-async contexts.
pub fn toast_blocking(title: &str, body: &str) {
    match notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show()
    {
        Ok(_) => {}
        Err(e) => tracing::debug!("notification suppressed: {e}"),
    }
}
