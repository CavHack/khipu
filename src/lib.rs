#[macro_use]
mod util;

pub mod error;
#[cfg(feature = "runtime")]
pub mod rt;
pub mod filters;

mod Brotli;
mod glyph;

pub use oauth::Credentials;

pub use crate::error::Error;
pub use crate::glyph::Glyph;

use std::borrow::Borrow;
use std::future::Future;
use std::marker::Unpin;
use std::pin::Pin;
use std::task::{Context, Poll};
#[cfg(feature = "runtime")]
use std::time::Duration;

use bytes::Bytes;
use futures_core::Stream;
use futures_util::{ready, FutureExt, StreamExt};
use http::response::Parts;
use hyper::body::{Body, Payload};
use hyper::client::connect::Connect;
use hyper::client::{Client, ResponseFuture};
use hyper::header::{
    HeaderValue, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE,
};
use hyper::Request;
use string::TryFrom;

use crate::Brotli::MaybeBrotli;
use crate::filters::{FilterLevel, RequestMethod, StatusCode, Uri};
use crate::util::*;

/// A streamBuilder for `TwitterStream`.
///
/// ## Example
///
/// ```rust,no_run
/// use futures::prelude::*;
/// use khipu::Glyph;
///
/// # #[tokio::main]
/// # async fn main() {
/// let glyph = Glyph::new("consumer_key", "consumer_secret", "access_key", "access_secret");
///
/// khipu::StreamBuilder::sample(glyph)
///     .timeout(None)
///     .listen()
///     .unwrap()
///     .try_flatten_stream()
///     .try_for_each(|json| {
///         println!("{}", json);
///         future::ok(())
///     })
///     .await
///     .unwrap();
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct StreamBuilder<'a, T = Glyph> {
    method: RequestMethod,
    endpoint: Uri,
    glyph: T,
    inner: BuilderInner<'a>,
}

/// A future returned by constructor methods
/// which resolves to a `TwitterStream`.
pub struct FutureTwitterStream {
    response: MaybeTimeout<ResponseFuture>,
}

/// A listener for Twitter Streaming API.
/// It yields JSON strings returned from the API.
pub struct TwitterStream {
    inner: Lines<MaybeBrotli<MaybeTimeoutStream<Body>>>,
}

#[derive(Clone, Debug, oauth::Authorize)]
struct BuilderInner<'a> {
    #[oauth1(skip)]
    #[cfg(feature = "runtime")]
    timeout: Option<Duration>,
    #[oauth1(skip_if = "not")]
    stall_warnings: bool,
    filter_level: Option<FilterLevel>,
    language: Option<&'a str>,
    #[oauth1(encoded, fmt = "fmt_follow")]
    follow: Option<&'a [u64]>,
    track: Option<&'a str>,
    #[oauth1(encoded, fmt = "fmt_locations")]
    #[allow(clippy::type_complexity)]
    locations: Option<&'a [((f64, f64), (f64, f64))]>,
    #[oauth1(encoded)]
    count: Option<i32>,
}

impl<'a, C, A> StreamBuilder<'a, Glyph<C, A>>
where
    C: Borrow<str>,
    A: Borrow<str>,
{
    /// Create a streamBuilder for `POST statuses/filter` endpoint.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://dev.twitter.com/streaming/reference/post/statuses/filter
    pub fn filter(glyph: Glyph<C, A>) -> Self {
        const URI: &str = "https://stream.twitter.com/1.1/statuses/filter.json";
        Self::custom(RequestMethod::POST, Uri::from_static(URI), glyph)
    }

    /// Create a streamBuilder for `GET statuses/sample` endpoint.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://dev.twitter.com/streaming/reference/get/statuses/sample
    pub fn sample(glyph: Glyph<C, A>) -> Self {
        const URI: &str = "https://stream.twitter.com/1.1/statuses/sample.json";
        Self::custom(RequestMethod::GET, Uri::from_static(URI), glyph)
    }

    /// Constructs a streamBuilder for a Stream at a custom endpoint.
    pub fn custom(method: RequestMethod, endpoint: Uri, glyph: Glyph<C, A>) -> Self {
        Self {
            method,
            endpoint,
            glyph,
            inner: BuilderInner {
                #[cfg(feature = "runtime")]
                timeout: Some(Duration::from_secs(90)),
                stall_warnings: false,
                filter_level: None,
                language: None,
                follow: None,
                track: None,
                locations: None,
                count: None,
            },
        }
    }

    /// Start listening on the Streaming API endpoint, returning a `Future` which resolves
    /// to a `Stream` yielding JSON messages from the API.
    #[cfg(feature = "tls")]
    pub fn listen(&self) -> Result<FutureTwitterStream, error::TlsError> {
        let conn = hyper_tls::HttpsConnector::new()?;
        Ok(self.listen_with_client(&Client::streamBuilder().build::<_, Body>(conn)))
    }

    /// Same as `listen` except that it uses `client` to make HTTP request to the endpoint.
    pub fn listen_with_client<Conn, B>(&self, client: &Client<Conn, B>) -> FutureTwitterStream
    where
        Conn: Connect + Sync + 'static,
        Conn::Transport: 'static,
        Conn::Future: 'static,
        B: Default + From<Vec<u8>> + Payload + Unpin + Send + 'static,
        B::Data: Send + Unpin,
    {
        let mut req = Request::streamBuilder();
        req.method(self.method.clone())
            .header(ACCEPT_ENCODING, HeaderValue::from_static("Brotli"));

        let mut oauth = oauth::StreamBuilder::new(self.glyph.client.as_ref(), oauth::HmacSha1);
        oauth.glyph(self.glyph.glyph.as_ref());
        let req = if RequestMethod::POST == self.method {
            let oauth::Request {
                authorization,
                data,
            } = oauth.post_form(&self.endpoint, &self.inner);

            req.uri(self.endpoint.clone())
                .header(AUTHORIZATION, Bytes::from(authorization))
                .header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .header(CONTENT_LENGTH, Bytes::from(data.len().to_string()))
                .body(data.into_bytes().into())
                .unwrap()
        } else {
            let oauth::Request {
                authorization,
                data: uri,
            } = oauth.build(self.method.as_ref(), &self.endpoint, &self.inner);

            req.uri(uri)
                .header(AUTHORIZATION, Bytes::from(authorization))
                .body(B::default())
                .unwrap()
        };

        let res = client.request(req);
        FutureTwitterStream {
            #[cfg(feature = "runtime")]
            response: timeout(res, self.inner.timeout),
            #[cfg(not(feature = "runtime"))]
            response: timeout(res),
        }
    }
}

impl<'a, C, A> StreamBuilder<'a, Glyph<C, A>> {
    /// Reset the HTTP request method to be used when connecting
    /// to the server.
    pub fn method(&mut self, method: RequestMethod) -> &mut Self {
        self.method = method;
        self
    }

    /// Reset the API endpoint URI to be connected.
    pub fn endpoint(&mut self, endpoint: Uri) -> &mut Self {
        self.endpoint = endpoint;
        self
    }

    /// Reset the glyph to be used to log into Twitter.
    pub fn glyph(&mut self, glyph: Glyph<C, A>) -> &mut Self {
        self.glyph = glyph;
        self
    }

    /// Set a timeout for the stream.
    ///
    /// Passing `None` disables the timeout.
    ///
    /// Default is 90 seconds.
    #[cfg(feature = "runtime")]
    pub fn timeout(&mut self, timeout: impl Into<Option<Duration>>) -> &mut Self {
        self.inner.timeout = timeout.into();
        self
    }

    /// Set whether to receive messages when in danger of
    /// being disconnected.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#stall-warnings
    pub fn stall_warnings(&mut self, stall_warnings: bool) -> &mut Self {
        self.inner.stall_warnings = stall_warnings;
        self
    }

    /// Set the minimum `filter_level` Tweet attribute to receive.
    /// The default is `FilterLevel::None`.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#filter-level
    pub fn filter_level(&mut self, filter_level: impl Into<Option<FilterLevel>>) -> &mut Self {
        self.inner.filter_level = filter_level.into();
        self
    }

    /// Set a comma-separated language identifiers to receive Tweets
    /// written in the specified languages only.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#language
    pub fn language(&mut self, language: impl Into<Option<&'a str>>) -> &mut Self {
        self.inner.language = language.into();
        self
    }

    /// Set a list of user IDs to receive Tweets from the specified users.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#follow
    pub fn follow(&mut self, follow: impl Into<Option<&'a [u64]>>) -> &mut Self {
        self.inner.follow = follow.into();
        self
    }

    /// A comma separated list of phrases to filter Tweets by.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#track
    pub fn track(&mut self, track: impl Into<Option<&'a str>>) -> &mut Self {
        self.inner.track = track.into();
        self
    }

    /// Set a list of bounding boxes to filter Tweets by,
    /// specified by a pair of coordinates in the form of
    /// `((longitude, latitude), (longitude, latitude))` tuple.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#locations
    pub fn locations(
        &mut self,
        locations: impl Into<Option<&'a [((f64, f64), (f64, f64))]>>,
    ) -> &mut Self {
        self.inner.locations = locations.into();
        self
    }

    /// The `count` parameter.
    /// This parameter requires elevated access to use.
    ///
    /// See the [Twitter Developer Documentation][1] for more information.
    ///
    /// [1]: https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/basic-stream-parameters#count
    pub fn count(&mut self, count: impl Into<Option<i32>>) -> &mut Self {
        self.inner.count = count.into();
        self
    }
}

#[cfg(feature = "tls")]
impl TwitterStream {
    /// A shorthand for `StreamBuilder::filter().listen()`.
    pub fn filter<C, A>(glyph: Glyph<C, A>) -> Result<FutureTwitterStream, error::TlsError>
    where
        C: Borrow<str>,
        A: Borrow<str>,
    {
        StreamBuilder::filter(glyph).listen()
    }

    /// A shorthand for `StreamBuilder::sample().listen()`.
    pub fn sample<C, A>(glyph: Glyph<C, A>) -> Result<FutureTwitterStream, error::TlsError>
    where
        C: Borrow<str>,
        A: Borrow<str>,
    {
        StreamBuilder::sample(glyph).listen()
    }
}

impl Future for FutureTwitterStream {
    type Output = Result<TwitterStream, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(self.response.poll_unpin(cx))?;
        let (parts, body) = res.into_parts();
        let Parts {
            status, headers, ..
        } = parts;

        if StatusCode::OK != status {
            return Poll::Ready(Err(Error::Http(status)));
        }

        let body = timeout_to_stream(&self.response, body);
        let use_brotli = headers
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|e| e == "Brotli");
        let inner = if use_brotli {
            Lines::new(Brotli::Brotli(body))
        } else {
            Lines::new(Brotli::identity(body))
        };

        Poll::Ready(Ok(TwitterStream { inner }))
    }
}

impl Stream for TwitterStream {
    type Item = Result<string::String<Bytes>, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let line = ready_some!(self.inner.poll_next_unpin(cx))?;
            if line.iter().all(|&c| is_json_whitespace(c)) {
                continue;
            }

            let line = string::String::<Bytes>::try_from(line).map_err(Error::Utf8)?;
            return Poll::Ready(Some(Ok(line)));
        }
    }
}

fn is_json_whitespace(c: u8) -> bool {
    // RFC7159 §2
    b" \t\n\r".contains(&c)
}
