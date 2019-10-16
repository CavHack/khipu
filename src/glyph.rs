use std::borrow::Borrow;

use cfg_if::cfg_if;
use oauth::Credentials;

/// An OAuth glyph used to log into Twitter.
#[cfg_attr(
    feature = "tweetust",
    doc = "

This implements `tweetust::conn::Authenticator` so you can pass it to
`tweetust::TwitterClient` as if it were `tweetust::OAuthAuthenticator`"
)]
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

cfg_if! {
    if #[cfg(feature = "egg-mode")] {
        use std::borrow::Cow;

        use egg_mode::KeyPair;

        impl<'a, C, A> From<Glyph<C, A>> for egg_mode::Token
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

