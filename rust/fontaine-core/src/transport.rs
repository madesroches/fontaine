//! An HTTP client for the model providers that keeps the transport cause in the error text.
//!
//! rig's `ProviderError::Http` has no `source()`, so a refused connection reads as "Http client
//! error: error sending request" with the reason cut off one level down. The wrapper here moves
//! that reason into the message, which is what rig relays into its error reports.

use std::error::Error as StdError;
use std::fmt;

use fontaine_tools::error_chain::chain;
use futures::StreamExt;
use rig_core::http_client::{
    Bytes, Error, HttpClientExt, LazyBody, MultipartForm, Request, Response, Result,
    StreamingResponse,
};
use rig_reqwest::ReqwestClient;

type Cause = Box<dyn StdError + Send + Sync + 'static>;

/// A transport error whose message carries its own causes. The variant stays `Instance`, so
/// rig's retry classification is unchanged.
#[derive(Debug)]
struct WithCauses(Cause);

impl fmt::Display for WithCauses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&chain(&*self.0))
    }
}

impl StdError for WithCauses {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.source()
    }
}

/// Request URLs are dropped because some providers put the API key in the query string.
fn name_cause(error: Error) -> Error {
    match error {
        Error::Instance(inner) => {
            let inner: Cause = match inner.downcast::<reqwest::Error>() {
                Ok(e) => Box::new(e.without_url()),
                Err(other) => other,
            };
            Error::Instance(Box::new(WithCauses(inner)))
        }
        other => other,
    }
}

/// Delegates to `C`, naming the cause of every transport error it returns.
#[derive(Clone, Debug)]
pub struct CausedClient<C = ReqwestClient>(C);

impl<C> CausedClient<C> {
    pub fn new(inner: C) -> Self {
        Self(inner)
    }
}

impl<C: HttpClientExt> HttpClientExt for CausedClient<C> {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let sent = self.0.send::<T, U>(req);
        async move { sent.await.map(named_body).map_err(name_cause) }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        let sent = self.0.send_multipart::<U>(req);
        async move { sent.await.map(named_body).map_err(name_cause) }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let sent = self.0.send_streaming::<T>(req);
        async move {
            let response = sent.await.map_err(name_cause)?;
            // A reset mid-stream fails in a chunk, not in the send.
            Ok(response.map(|body| {
                let body: rig_core::http_client::BoxedStream = Box::pin(body.map(name_chunk));
                body
            }))
        }
    }
}

#[allow(clippy::result_large_err)]
fn name_chunk(chunk: Result<Bytes>) -> Result<Bytes> {
    chunk.map_err(name_cause)
}

fn named_body<U: Send + 'static>(response: Response<LazyBody<U>>) -> Response<LazyBody<U>> {
    response.map(|body| {
        let body: LazyBody<U> = Box::pin(async move { body.await.map_err(name_cause) });
        body
    })
}

/// A client for one account. Per account rather than rig's process-wide one, so no pooled
/// connection outlives the runtime that made it. Mirrors rig's default build, including its
/// fallback to the bundled roots on a host with no system CA certificates.
pub fn caused_client() -> std::result::Result<CausedClient, String> {
    let built = reqwest::Client::builder()
        .build()
        .or_else(with_bundled_roots)
        .map_err(|e| {
            format!(
                "could not build the HTTP client: {}",
                chain(&e.without_url())
            )
        })?;
    Ok(CausedClient::new(ReqwestClient::from(built)))
}

fn with_bundled_roots(error: reqwest::Error) -> reqwest::Result<reqwest::Client> {
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    let Ok(client) = reqwest::Client::builder().tls_certs_only(roots).build() else {
        return Err(error);
    };
    tracing::warn!(
        "the system's CA certificates could not be loaded ({}), so the bundled Mozilla roots \
         are trusted; install the system's CA certificates, or set SSL_CERT_FILE or \
         SSL_CERT_DIR, to trust those instead",
        chain(&error)
    );
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Root;
    impl fmt::Display for Root {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Connection refused (os error 111)")
        }
    }
    impl StdError for Root {}

    #[derive(Debug)]
    struct Outer(Root);
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("error sending request")
        }
    }
    impl StdError for Outer {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }

    fn instance() -> Error {
        Error::Instance(Box::new(Outer(Root)))
    }

    enum Fails {
        Send,
        Chunk,
    }

    struct Fake(Fails);

    #[allow(clippy::manual_async_fn)]
    impl HttpClientExt for Fake {
        fn send<T, U>(
            &self,
            _req: Request<T>,
        ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
        where
            T: Into<Bytes> + Send,
            U: From<Bytes> + Send + 'static,
        {
            async { Err(instance()) }
        }

        fn send_multipart<U>(
            &self,
            _req: Request<MultipartForm>,
        ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
        where
            U: From<Bytes> + Send + 'static,
        {
            async { Err(instance()) }
        }

        fn send_streaming<T>(
            &self,
            _req: Request<T>,
        ) -> impl Future<Output = Result<StreamingResponse>> + Send
        where
            T: Into<Bytes> + Send,
        {
            let fails_in_send = matches!(self.0, Fails::Send);
            async move {
                if fails_in_send {
                    return Err(instance());
                }
                let chunks: Vec<Result<Bytes>> =
                    vec![Ok(Bytes::from_static(b"ok")), Err(instance())];
                let body: rig_core::http_client::BoxedStream =
                    Box::pin(futures::stream::iter(chunks));
                Ok(Response::new(body))
            }
        }
    }

    fn named(error: &Error) -> String {
        error.to_string()
    }

    #[test]
    fn with_causes_displays_the_chain_and_keeps_the_source() {
        let error = name_cause(instance());
        assert_eq!(
            named(&error),
            "Http client error: error sending request: Connection refused (os error 111)"
        );
        let Error::Instance(inner) = &error else {
            panic!("the variant must stay Instance");
        };
        assert_eq!(
            inner.source().map(ToString::to_string).as_deref(),
            Some("Connection refused (os error 111)")
        );
    }

    #[test]
    fn other_variants_pass_through() {
        assert!(matches!(name_cause(Error::StreamEnded), Error::StreamEnded));
    }

    #[tokio::test]
    async fn a_send_failure_names_its_cause() {
        let client = CausedClient::new(Fake(Fails::Send));
        let req = Request::new(Bytes::new());
        let Err(error) = client.send::<Bytes, Bytes>(req).await else {
            panic!("expected a failure");
        };
        assert!(named(&error).contains("Connection refused"), "{error}");
        let req = Request::new(Bytes::new());
        let Err(error) = client.send_streaming(req).await else {
            panic!("expected a failure");
        };
        assert!(named(&error).contains("Connection refused"), "{error}");
    }

    #[tokio::test]
    async fn a_failing_stream_chunk_names_its_cause() {
        let client = CausedClient::new(Fake(Fails::Chunk));
        let response = client
            .send_streaming(Request::new(Bytes::new()))
            .await
            .expect("the send succeeds");
        let mut body = response.into_body();
        assert!(body.next().await.expect("a chunk").is_ok());
        let error = body.next().await.expect("a chunk").expect_err("fails");
        assert!(named(&error).contains("Connection refused"), "{error}");
    }

    #[tokio::test]
    async fn a_real_transport_error_drops_the_request_url() {
        // Nothing listens on port 1.
        let url = "http://127.0.0.1:1/v1?key=SECRETKEY";
        let error = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("SECRETKEY"));
        let rewritten = name_cause(Error::Instance(Box::new(error)));
        let text = named(&rewritten);
        assert!(
            !text.contains("SECRETKEY") && !text.contains("127.0.0.1:1/v1"),
            "{text}"
        );
        assert!(text.contains("tcp connect error"), "{text}");
    }
}
