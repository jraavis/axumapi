//! Method routers: `get(handler).post(other)`.

use axum::handler::Handler;

/// Maps HTTP methods to handlers for a single path.
#[derive(Debug, Clone)]
pub struct MethodRouter(pub(crate) axum::routing::MethodRouter);

macro_rules! method_routes {
    ($($name:ident => $verb:literal),+ $(,)?) => {
        $(
            #[doc = concat!("Route `", $verb, "` requests to `handler`.")]
            pub fn $name<H, T>(handler: H) -> MethodRouter
            where
                H: Handler<T, ()>,
                T: 'static,
            {
                MethodRouter(axum::routing::$name(handler))
            }
        )+

        impl MethodRouter {
            $(
                #[doc = concat!("Additionally route `", $verb, "` requests to `handler`.")]
                #[must_use]
                pub fn $name<H, T>(self, handler: H) -> Self
                where
                    H: Handler<T, ()>,
                    T: 'static,
                {
                    Self(self.0.$name(handler))
                }
            )+
        }
    };
}

method_routes! {
    get => "GET",
    post => "POST",
    put => "PUT",
    patch => "PATCH",
    delete => "DELETE",
    head => "HEAD",
    options => "OPTIONS",
}
