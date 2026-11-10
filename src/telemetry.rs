use std::{error::Error, fmt::Write, time::Duration};

use axum::{
    Router,
    http::{HeaderName, Request, Response},
};
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::{Span, info, info_span};
use tracing_subscriber::EnvFilter;

/// Used when `RUST_LOG` isn't set. Keeps dependencies' warnings and errors.
const DEFAULT_FILTER: &str = "warn,mailman=info";

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Logs to stderr, filtered by `RUST_LOG` (e.g. `RUST_LOG=mailman=debug`).
/// Set `LOG_FORMAT=json` for one JSON object per line, for log collectors.
pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if std::env::var("LOG_FORMAT").is_ok_and(|format| format == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// Gives each request an ID (keeping one the client sent in `x-request-id`),
/// returns it in the response, and logs the request inside a span carrying
/// it, so every line a request logs can be tied together. Logs one line per
/// response with its status and the latency to the response headers, which
/// for a streamed completion is time to first token, not the whole stream.
pub fn http_layers<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router.layer(
        ServiceBuilder::new()
            .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(|req: &Request<_>| {
                        let request_id = req
                            .headers()
                            .get(REQUEST_ID_HEADER)
                            .and_then(|id| id.to_str().ok())
                            .unwrap_or_default();
                        info_span!(
                            "request",
                            method = %req.method(),
                            path = req.uri().path(),
                            request_id,
                        )
                    })
                    // Logged here rather than with tower-http's defaults so the
                    // line is under mailman's target, which `DEFAULT_FILTER`
                    // shows at info.
                    .on_response(|res: &Response<_>, latency: Duration, _: &Span| {
                        info!(status = res.status().as_u16(), ?latency, "response")
                    })
                    // 5xx responses are in the line above, and their causes are
                    // logged where they happen. tower-http's error for each one
                    // would flood the logs during an outage.
                    .on_failure(()),
            )
            .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER)),
    )
}

/// Joins an error with its sources. reqwest's own message is only "error
/// sending request for url (...)", and the useful part, like "connection
/// refused", is in the sources.
pub fn error_chain(err: &(dyn Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        let _ = write!(message, ": {err}");
        source = err.source();
    }
    message
}
