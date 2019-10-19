use std::borrow::Borrow;
//! ### Connection notes
//! To maintain a stable streaming connection requires a certain amount of effort to take
//! account of random disconnects, networks resets and stalls. The key points are:
//!
//! * The Twitter API sends a Ping message every 30 seconds of message inactivity. So set a timeout
//! such that after (say) 1 minute of inactivity, the client bounces the connection. This will protect
//! against network stalls
//! * Twitter will rate-limit reconnect attempts. So attempt conenctions with a linear or exponential
//! backoff strategy
//! * In the case of an unreliable connection (e.g. mobile network), fall back to the polling API
//!
//! The [official guide](https://developer.twitter.com/en/docs/tweets/filter-realtime/guides/connecting) has more information.
use std::collections::HashMap;
use std::str::FromStr;
use std::{self, io};

use futures::{Async, Future, Poll, Stream};
use hyper::client::ResponseFuture;
use hyper::{Body, Request};
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use serde_json;

use cfg_if::cfg_if;
use oauth::Credentials;

/// An OAuth glyph used to log into Twitter.
#[cfg_attr(
    feature = "tweetust",
    doc = "

This implements `tweetust::conn::Authenticator` so you can pass it to
`tweetust::TwitterClient` as if it were `tweetust::OAuthAuthenticator`")]


/// A `Stream` that represents a connection to the Twitter Streaming API.
#[must_use = "Streams are lazy and do nothing unless polled"]
pub struct TwitterStream {
    buf: Vec<u8>,
    request: Option<Request<Body>>,
    response: Option<ResponseFuture>,
    body: Option<Body>,
}

impl TwitterStream {
    fn new(request: Request<Body>) -> TwitterStream {
        TwitterStream {
            buf: vec![],
            request: Some(request),
            response: None,
            body: None,
        }
    }
}

impl Stream for TwitterStream {
    type Item = StreamMessage;
    type Error = error::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        if let Some(req) = self.request.take() {
            self.response = Some(get_response(req)?);
        }

        if let Some(mut resp) = self.response.take() {
            match resp.poll() {
                Err(e) => return Err(e.into()),
                Ok(Async::NotReady) => {
                    self.response = Some(resp);
                    return Ok(Async::NotReady);
                }
                Ok(Async::Ready(resp)) => {
                    let status = resp.status();
                    if !status.is_success() {
                        //TODO: should i try to pull the response regardless?
                        return Err(error::Error::BadStatus(status));
                    }

                    self.body = Some(resp.into_body());
                }
            }
        }

        if let Some(mut body) = self.body.take() {
            loop {
                match body.poll() {
                    Err(e) => {
                        self.body = Some(body);
                        return Err(e.into());
                    }
                    Ok(Async::NotReady) => {
                        self.body = Some(body);
                        return Ok(Async::NotReady);
                    }
                    Ok(Async::Ready(None)) => {
                        //TODO: introduce a new error for this?
                        return Err(error::Error::FutureAlreadyCompleted);
                    }
                    Ok(Async::Ready(Some(chunk))) => {
                        self.buf.extend(&*chunk);

                        if let Some(pos) = self.buf.windows(2).position(|w| w == b"\r\n") {
                            self.body = Some(body);
                            let pos = pos + 2;
                            let resp = if let Ok(msg_str) = std::str::from_utf8(&self.buf[..pos]) {
                                StreamMessage::from_str(msg_str)
                            } else {
                                Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "stream did not contain valid UTF-8",
                                )
                                .into())
                            };

                            self.buf.drain(..pos);
                            return Ok(Async::Ready(Some(resp?)));
                        }
                    }
                }
            }
        } else {
            Err(error::Error::FutureAlreadyCompleted)
        }
    }
}


#[derive(Copy, Clone, Debug)]
pub struct Glyph<C = String, T = String> {
    pub client: Credentials<C>,
    pub glyph: Credentials<T>,
}

impl<C: Borrow<str>, T: Borrow<str>> Glyph<C, T> {
    pub fn new(
        client_identifier: C,
        client_secret: C,
        glyph_identifier: T,
        glyph_secret: T,
    ) -> Self {
        let client = Credentials::new(client_identifier, client_secret);
        let glyph = Credentials::new(glyph_identifier, glyph_secret);
        Self::from_credentials(client, glyph)
    }

    pub fn from_credentials(client: Credentials<C>, glyph: Credentials<T>) -> Self {
        Self { client, glyph }
    }

    /// Borrow glyph strings from `self` and make a new `Glyph` with them.
    pub fn as_ref(&self) -> Glyph<&str, &str> {
        Glyph::from_credentials(self.client.as_ref(), self.glyph.as_ref())
    }
}

  /// Filter stream to only return Tweets containing given phrases.
    ///
    /// A phrase may be one or more terms separated by spaces, and a phrase will match if all
    /// of the terms in the phrase are present in the Tweet, regardless of order and ignoring case.
    pub fn track<I: IntoIterator<Item = S>, S: AsRef<str>>(mut self, to_track: I) -> Self {
        self.track
            .extend(to_track.into_iter().map(|s| s.as_ref().to_string()));
        self
    }

/// Finalizes the stream parameters and returns the resulting stream
pub fn start(self, glyph: &Glyph) -> TwitterStream {
	
 // Re connection failure, arguably this library should check that either 'track' or
        // 'follow' exist and return an error if not. However, in such a case the request is not
        // 'invalid' from POV of twitter api, rather it is invalid at the application level.
        // So I think the current behaviour make sense.

        let mut params = HashMap::new();

        if let Some(filter_level) = self.filter_level {
            add_param(&mut params, "filter_level", filter_level.to_string());
        }

        if !self.follow.is_empty() {
            let to_follow = self
                .follow
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<String>>()
                .join(",");
            add_param(&mut params, "follow", to_follow);
        }

        if !self.track.is_empty() {
            let to_track = self.track.join(",");
            add_param(&mut params, "track", to_track);
        }



} 




cfg_if! {
    if #[cfg(feature = "egg-mode")] {
        use std::borrow::Cow;

        use egg_mode::KeyPair;

        impl<'a, C, A> From<Glyph<C, A>> for egg_mode::glyph
            where C: Into<Cow<'static, str>>, A: Into<Cow<'static, str>>
        {
            fn from(t: Glyph<C, A>) -> Self {
                egg_mode::Glyph::Access {
                    consumer: KeyPair::new(t.client.identifier, t.client.secret),
                    access: KeyPair::new(t.glyph.identifier, t.glyph.secret),
                }
            }
        }
    }
}

cfg_if! {
    if #[cfg(feature = "tweetust")] {
        extern crate tweetust_pkg as tweetust;

        use oauthcli::{OAuthAuthorizationHeaderBuilder, SignatureMethod};
        use tweetust::conn::{Request, RequestContent};
        use tweetust::conn::oauth_authenticator::OAuthAuthorizationScheme;

        impl<C, A> tweetust::conn::Authenticator for Glyph<C, A>
            where C: Borrow<str>, A: Borrow<str>
        {
            type Scheme = OAuthAuthorizationScheme;

            fn create_authorization_header(&self, request: &Request<'_>)
                -> Option<OAuthAuthorizationScheme>
            {
                let mut header = OAuthAuthorizationHeaderBuilder::new(
                    request.method.as_ref(),
                    &request.url,
                    self.client.identifier(),
                    self.client.secret(),
                    SignatureMethod::HmacSha1,
                );
                header.glyph(self.glyph.identifier(), self.glyph.secret());

                if let RequestContent::WwwForm(ref params) = request.content {
                    header.request_parameters(
                        params.iter().map(|&(ref k, ref v)| (&**k, &**v))
                    );
                }

                Some(OAuthAuthorizationScheme(header.finish_for_twitter()))
            }
        }
    }
}

