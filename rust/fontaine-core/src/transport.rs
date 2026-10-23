//! An HTTP client for the model providers that keeps the transport cause in the error text.
//!
//! rig's `ProviderError::Http` has no `source()`, so a refused connection reads as "Http client
//! error: error sending request" with the reason cut off one level down. The wrapper here moves
//! that reason into the message, which is what rig relays into its error reports.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use fontaine_tools::error_chain::chain;
use futures::StreamExt;
use http::header::{AUTHORIZATION, HeaderName, USER_AGENT};
use rig_core::http_client::{
    Bytes, Error, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, Response, Result,
    StreamingResponse, Uri,
};
use rig_reqwest::ReqwestClient;

use crate::providers::token_source::TokenSource;

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
    Ok(CausedClient::new(ReqwestClient::from(reqwest_client()?)))
}

/// The plain client behind [`caused_client`], for requests that do not go through rig.
pub fn reqwest_client() -> std::result::Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .build()
        .or_else(with_bundled_roots)
        .map_err(|e| {
            format!(
                "could not build the HTTP client: {}",
                chain(&e.without_url())
            )
        })
}

/// How an access token is put on a request, replacing the placeholder key rig put there.
#[derive(Clone, Debug)]
pub enum AuthPlacement {
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// The token alone in this header, with no `Authorization`.
    Header(HeaderName),
    /// Gemini's `?key=` becomes `x-goog-api-key`, so the token never sits in a URL.
    GoogleKey,
}

/// What [`Decorated`] adds to every request.
#[derive(Clone, Default)]
pub struct Decoration {
    pub user_agent: Option<&'static str>,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub auth: Option<(Arc<TokenSource>, AuthPlacement)>,
}

/// Delegates to `C` after decorating the request. Headers are replaced, never appended, so what
/// rig set (the placeholder key) cannot survive next to the real one.
#[derive(Clone)]
pub struct Decorated<C = CausedClient> {
    inner: C,
    decoration: Arc<Decoration>,
}

impl<C> Decorated<C> {
    pub fn new(inner: C, decoration: Decoration) -> Self {
        Decorated {
            inner,
            decoration: Arc::new(decoration),
        }
    }
}

#[allow(clippy::result_large_err)]
fn token_header(value: &str) -> Result<HeaderValue> {
    let mut header = HeaderValue::from_str(value).map_err(|_| {
        Error::Instance(Box::<dyn StdError + Send + Sync>::from(
            "the access token cannot be sent as an HTTP header",
        ))
    })?;
    header.set_sensitive(true);
    Ok(header)
}

#[allow(clippy::result_large_err)]
fn without_key_param(uri: &Uri) -> Result<Uri> {
    let Some(query) = uri.query() else {
        return Ok(uri.clone());
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| pair.split('=').next() != Some("key"))
        .collect();
    let mut path = uri.path().to_string();
    if !kept.is_empty() {
        path.push('?');
        path.push_str(&kept.join("&"));
    }
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(
        path.parse()
            .map_err(|e: http::uri::InvalidUri| Error::Protocol(e.into()))?,
    );
    Uri::from_parts(parts).map_err(|e| Error::Protocol(e.into()))
}

#[allow(clippy::result_large_err)]
async fn decorate<B>(decoration: &Decoration, mut req: Request<B>) -> Result<Request<B>> {
    if let Some(agent) = decoration.user_agent {
        req.headers_mut()
            .insert(USER_AGENT, HeaderValue::from_static(agent));
    }
    for (name, value) in &decoration.headers {
        req.headers_mut().insert(name.clone(), value.clone());
    }
    if let Some((source, placement)) = &decoration.auth {
        let token = source
            .token()
            .await
            .map_err(|e| Error::Instance(Box::new(e)))?;
        let headers = req.headers_mut();
        match placement {
            AuthPlacement::Bearer => {
                headers.insert(AUTHORIZATION, token_header(&format!("Bearer {token}"))?);
            }
            AuthPlacement::Header(name) => {
                headers.remove(AUTHORIZATION);
                headers.insert(name.clone(), token_header(&token)?);
            }
            AuthPlacement::GoogleKey => {
                headers.remove(AUTHORIZATION);
                headers.insert(
                    HeaderName::from_static("x-goog-api-key"),
                    token_header(&token)?,
                );
                *req.uri_mut() = without_key_param(req.uri())?;
            }
        }
    }
    Ok(req)
}

impl<C: HttpClientExt + Clone + 'static> HttpClientExt for Decorated<C> {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let req = req.map(Into::<Bytes>::into);
        let (inner, decoration) = (self.inner.clone(), self.decoration.clone());
        async move {
            let req = decorate(&decoration, req).await?;
            inner.send::<Bytes, U>(req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        let (inner, decoration) = (self.inner.clone(), self.decoration.clone());
        async move {
            let req = decorate(&decoration, req).await?;
            inner.send_multipart::<U>(req).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let req = req.map(Into::<Bytes>::into);
        let (inner, decoration) = (self.inner.clone(), self.decoration.clone());
        async move {
            let req = decorate(&decoration, req).await?;
            inner.send_streaming::<Bytes>(req).await
        }
    }
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

#[cfg(test)]
mod decoration_tests {
    use std::sync::{Arc, Mutex};

    use futures::future::BoxFuture;
    use rig_core::http_client::HeaderMap;

    use super::*;
    use crate::credentials::OAuthTokens;
    use crate::providers::device_flow::RefreshError;
    use crate::providers::token_source::{Refresher, now_unix};

    type Seen = Arc<Mutex<Vec<(Uri, HeaderMap)>>>;

    #[derive(Clone, Default)]
    struct Capture(Seen);

    #[allow(clippy::manual_async_fn)]
    impl HttpClientExt for Capture {
        fn send<T, U>(
            &self,
            req: Request<T>,
        ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
        where
            T: Into<Bytes> + Send,
            U: From<Bytes> + Send + 'static,
        {
            self.0
                .lock()
                .unwrap()
                .push((req.uri().clone(), req.headers().clone()));
            async {
                let body: LazyBody<U> = Box::pin(async { Ok(U::from(Bytes::new())) });
                Ok(Response::new(body))
            }
        }

        fn send_multipart<U>(
            &self,
            _req: Request<MultipartForm>,
        ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
        where
            U: From<Bytes> + Send + 'static,
        {
            async { Err(Error::StreamEnded) }
        }

        fn send_streaming<T>(
            &self,
            req: Request<T>,
        ) -> impl Future<Output = Result<StreamingResponse>> + Send
        where
            T: Into<Bytes> + Send,
        {
            self.0
                .lock()
                .unwrap()
                .push((req.uri().clone(), req.headers().clone()));
            async {
                let body: rig_core::http_client::BoxedStream = Box::pin(futures::stream::empty());
                Ok(Response::new(body))
            }
        }
    }

    struct SignedOut;

    impl Refresher for SignedOut {
        fn refresh<'a>(
            &'a self,
            _current: &'a OAuthTokens,
        ) -> BoxFuture<'a, std::result::Result<OAuthTokens, RefreshError>> {
            Box::pin(async { Err(RefreshError::SignedOut) })
        }
    }

    fn source(dir: &std::path::Path, expires_at: u64) -> Arc<TokenSource> {
        let tokens = OAuthTokens {
            access: "the-token".to_string(),
            refresh: "the-refresh".to_string(),
            expires_at,
            extra: Default::default(),
        };
        crate::credentials::store_oauth(dir, "go", &tokens).unwrap();
        TokenSource::new(dir, "go", tokens, Arc::new(SignedOut))
    }

    fn decorated(placement: AuthPlacement, source: Arc<TokenSource>) -> (Decorated<Capture>, Seen) {
        let capture = Capture::default();
        let seen = capture.0.clone();
        let decoration = Decoration {
            user_agent: Some("fontaine/test"),
            headers: vec![(
                HeaderName::from_static("x-opencode-session"),
                HeaderValue::from_static("sess-1"),
            )],
            auth: Some((source, placement)),
        };
        (Decorated::new(capture, decoration), seen)
    }

    async fn send(client: &Decorated<Capture>, uri: &str, placeholder: &[(&str, &str)]) {
        let mut req = Request::post(uri).body(Bytes::new()).unwrap();
        for (name, value) in placeholder {
            req.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        client.send::<Bytes, Bytes>(req).await.unwrap();
    }

    #[tokio::test]
    async fn user_agent_session_and_a_bearer_token_replace_what_rig_set() {
        let dir = tempfile::tempdir().unwrap();
        let (client, seen) =
            decorated(AuthPlacement::Bearer, source(dir.path(), now_unix() + 3600));
        send(
            &client,
            "http://gateway.test/openai/v1/chat/completions",
            &[
                ("authorization", "Bearer placeholder"),
                ("user-agent", "rig"),
            ],
        )
        .await;
        let seen = seen.lock().unwrap();
        let headers = &seen[0].1;
        assert_eq!(headers["user-agent"], "fontaine/test");
        assert_eq!(headers["x-opencode-session"], "sess-1");
        assert_eq!(headers.get_all("authorization").iter().count(), 1);
        assert_eq!(headers["authorization"], "Bearer the-token");
        assert!(headers["authorization"].is_sensitive());
    }

    #[tokio::test]
    async fn a_named_header_carries_the_token_and_authorization_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let placement = AuthPlacement::Header(HeaderName::from_static("x-api-key"));
        let (client, seen) = decorated(placement, source(dir.path(), now_unix() + 3600));
        send(
            &client,
            "http://gateway.test/anthropic/v1/messages",
            &[("authorization", "Bearer placeholder"), ("x-api-key", "")],
        )
        .await;
        let seen = seen.lock().unwrap();
        assert!(seen[0].1.get("authorization").is_none());
        assert_eq!(seen[0].1["x-api-key"], "the-token");
    }

    #[tokio::test]
    async fn the_google_key_moves_from_the_url_to_a_header() {
        let dir = tempfile::tempdir().unwrap();
        let (client, seen) = decorated(
            AuthPlacement::GoogleKey,
            source(dir.path(), now_unix() + 3600),
        );
        send(
            &client,
            "http://gateway.test/google/v1beta/models/m:generateContent?alt=sse&key=&x=1",
            &[],
        )
        .await;
        send(
            &client,
            "http://gateway.test/google/v1beta/models?key=placeholder",
            &[],
        )
        .await;
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].0.to_string(),
            "http://gateway.test/google/v1beta/models/m:generateContent?alt=sse&x=1"
        );
        assert_eq!(
            seen[1].0.to_string(),
            "http://gateway.test/google/v1beta/models"
        );
        for (_, headers) in seen.iter() {
            assert_eq!(headers["x-goog-api-key"], "the-token");
        }
    }

    #[tokio::test]
    async fn streaming_requests_are_decorated_too() {
        let dir = tempfile::tempdir().unwrap();
        let (client, seen) =
            decorated(AuthPlacement::Bearer, source(dir.path(), now_unix() + 3600));
        let req = Request::post("http://gateway.test/x")
            .body(Bytes::new())
            .unwrap();
        client.send_streaming(req).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].1["authorization"],
            "Bearer the-token"
        );
    }

    #[tokio::test]
    async fn a_token_that_cannot_be_had_is_a_transport_error_with_the_sign_in_hint() {
        let dir = tempfile::tempdir().unwrap();
        let (client, seen) = decorated(
            AuthPlacement::Bearer,
            source(dir.path(), now_unix().saturating_sub(10)),
        );
        let req = Request::post("http://gateway.test/x")
            .body(Bytes::new())
            .unwrap();
        let Err(error) = client.send::<Bytes, Bytes>(req).await else {
            panic!("expected a failure");
        };
        assert!(matches!(error, Error::Instance(_)));
        assert!(
            error.to_string().contains("fontaine auth login go"),
            "{error}"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "nothing is sent without a token"
        );
    }
}
