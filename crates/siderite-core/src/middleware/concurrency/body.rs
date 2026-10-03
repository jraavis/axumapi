//! Retain execution ownership without buffering response bodies.

use crate::{Body, Response};
use axum::body::HttpBody;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::OwnedSemaphorePermit;

type BodyPoll = Poll<Option<Result<Frame<Bytes>, axum::Error>>>;

struct RetainedBody {
    body: axum::body::Body,
    permit: Option<OwnedSemaphorePermit>,
}

impl HttpBody for RetainedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> BodyPoll {
        let this = self.get_mut();
        let result = Pin::new(&mut this.body).poll_frame(cx);
        let terminal = matches!(result, Poll::Ready(None | Some(Err(_))));
        if terminal || this.body.is_end_stream() {
            this.permit.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

pub(super) fn retain(r: Response, permit: OwnedSemaphorePermit) -> Response {
    r.map(|body| {
        let body = body.into_inner();
        let permit = (!body.is_end_stream()).then_some(permit);
        Body::from_inner(axum::body::Body::new(RetainedBody { body, permit }))
    })
}
