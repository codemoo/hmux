//! Response bodies retain their byte owners across Hyper's frame handoff.
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

pub struct Body(Kind);
enum Kind {
    One(Full<Bytes>),
    File(Box<crate::static_assets::FileBody>),
}
impl Body {
    pub fn new(bytes: Bytes) -> Self {
        Self(Kind::One(Full::new(bytes)))
    }
    pub(crate) fn file(body: crate::static_assets::FileBody) -> Self {
        Self(Kind::File(Box::new(body)))
    }
}
impl HttpBody for Body {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match &mut self.0 {
            Kind::One(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|frame| frame.map(|r| r.map_err(|never| match never {}))),
            Kind::File(body) => Pin::new(body.as_mut()).poll_frame(cx),
        }
    }
    fn is_end_stream(&self) -> bool {
        match &self.0 {
            Kind::One(b) => b.is_end_stream(),
            Kind::File(b) => b.is_end_stream(),
        }
    }
    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            Kind::One(b) => b.size_hint(),
            Kind::File(b) => b.size_hint(),
        }
    }
}
