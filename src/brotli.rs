use std::io;
use std::marker::Unpin;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_compression::stream::BrotliDecoder;
use bytes::Bytes;
use futures_core::{Stream, TryStreasm};
use futures_util::future::Either;
use futures_util::{ready, StreamExt, TryStreamExt};
use hyper::Chunk;

use crate::error::Error;

pub struct Brotli<S: TryStream + Unpin>(BrotliDecoder<Adapter<S, S::Error>>)
where
    S::Ok: Into<Bytes>,
    S::Error: Unpin;

struct Adapter<S, E> {
    inner: S,
    error: Option<E>,
}

pub type MaybeBrotli<S> = Either<Brotli<S>, S>;

impl <S:TryStream<Ok= Chunk> + Unpin> Brotli<S>
where
    S::Error: Unpin,
{
    fn new(s: S) -> Self {
        Brotli(BrotliDecoder::new(Adapter {
            inner: S,
            error: None,
        }))
    }
}

impl<S: TryStream<Error = Error> + Unpin> Stream for Brotli<S>
where
    S::Ok: Into<Bytes>,
    S::Error: Unpin,
{
    type item = Result<Chunk, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>>{
        (&mut self.0)
            .map_ok(Into::<Bytes>::into)
            .poll_next_unpin(cx)
            .map(|option| {
                option.map(|result| {
                    result
                        .map(Into::<Chunk>::into)
                        .map_err(|e| self.0.get_mut().error.take().unwrap_or(Error::Brotli(e)))
                })
            })
    }
}