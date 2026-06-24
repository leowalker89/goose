use tokio::task_local;

/// HTTP header used to propagate the ambient session id to upstream providers.
pub const SESSION_ID_HEADER: &str = "agent-session-id";

task_local! {
    static SESSION_ID: String;
}

/// Runs `f` with `session_id` set as the ambient session id, making it
/// available to provider request code (e.g. `Provider::stream`) via
/// [`current_session_id`] without threading it through every call.
///
/// This is generic request-scoped metadata; the providers crate does not
/// interpret its meaning beyond attaching it to outbound requests.
pub async fn with_session_id<F>(session_id: &str, f: F) -> F::Output
where
    F: std::future::Future,
{
    SESSION_ID.scope(session_id.to_string(), f).await
}

/// Returns the ambient session id set by [`with_session_id`], or an empty
/// string when none is in scope.
pub fn current_session_id() -> String {
    SESSION_ID.try_with(|id| id.clone()).unwrap_or_default()
}
