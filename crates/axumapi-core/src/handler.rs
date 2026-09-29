//! The [`Handler`] trait: async functions usable as endpoints.
//!
//! Implemented for `async fn`s and closures taking up to 12 extractor
//! arguments. Every argument but the last implements
//! [`FromRequestParts`]; the last implements [`FromRequest`] (and may read
//! the body). The return type implements [`IntoResponse`].
//!
//! Because extraction, response conversion and documentation all go through
//! axumapi traits, a handler's OpenAPI operation is derived from its
//! signature with no macros required.

use crate::extract::{FromRequest, FromRequestParts, Request};
use crate::response::{IntoResponse, Response};
use axumapi_openapi::{Operation, SchemaRegistry};
use std::future::Future;

/// An endpoint callable with a request. `T` is an inference marker.
pub trait Handler<T>: Clone + Send + Sync + Sized + 'static {
    /// Run extraction, the handler body and response conversion.
    fn call(self, req: Request) -> impl Future<Output = Response> + Send;

    /// Document the operation implied by the handler's signature.
    fn describe(op: &mut Operation, registry: &mut SchemaRegistry);
}

#[allow(clippy::manual_async_fn)]
impl<F, Fut, R> Handler<(R,)> for F
where
    F: FnOnce() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send,
    R: IntoResponse,
{
    fn call(self, _req: Request) -> impl Future<Output = Response> + Send {
        async move { self().await.into_response() }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        R::describe(op, registry);
    }
}

macro_rules! impl_handler {
    ($($p:ident),* ; $last:ident) => {
        #[allow(non_snake_case, unused_mut)]
        impl<F, Fut, R, $($p,)* $last> Handler<(R, $($p,)* $last)> for F
        where
            F: FnOnce($($p,)* $last) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = R> + Send,
            R: IntoResponse,
            $($p: FromRequestParts,)*
            $last: FromRequest,
        {
            fn call(self, req: Request) -> impl Future<Output = Response> + Send {
                async move {
                    let (mut parts, body) = req.into_parts();
                    $(
                        let $p = match $p::from_request_parts(&mut parts).await {
                            Ok(value) => value,
                            Err(err) => return err.into_response(),
                        };
                    )*
                    let req = Request::from_parts(parts, body);
                    let $last = match $last::from_request(req).await {
                        Ok(value) => value,
                        Err(err) => return err.into_response(),
                    };
                    self($($p,)* $last).await.into_response()
                }
            }

            fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
                $(<$p as FromRequestParts>::describe(op, registry);)*
                <$last as FromRequest>::describe(op, registry);
                R::describe(op, registry);
            }
        }
    };
}

impl_handler!(; A1);
impl_handler!(A1; A2);
impl_handler!(A1, A2; A3);
impl_handler!(A1, A2, A3; A4);
impl_handler!(A1, A2, A3, A4; A5);
impl_handler!(A1, A2, A3, A4, A5; A6);
impl_handler!(A1, A2, A3, A4, A5, A6; A7);
impl_handler!(A1, A2, A3, A4, A5, A6, A7; A8);
impl_handler!(A1, A2, A3, A4, A5, A6, A7, A8; A9);
impl_handler!(A1, A2, A3, A4, A5, A6, A7, A8, A9; A10);
impl_handler!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10; A11);
impl_handler!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11; A12);
