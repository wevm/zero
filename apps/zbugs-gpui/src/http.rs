//! The HTTP client GPUI loads images with, for avatars: blocking `ureq`
//! requests on their own threads. GPUI has none of its own.

use futures::FutureExt as _;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use gpui_kit::http_client::http::HeaderValue;
use gpui_kit::http_client::{AsyncBody, HttpClient, Request, Response, Url};
use std::time::Duration;

pub struct UreqClient(ureq::Agent);

impl UreqClient {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Self(config.into())
    }
}

impl HttpClient for UreqClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    /// GETs `req`'s URL, which is all images need.
    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let (agent, uri) = (self.0.clone(), req.uri().to_string());
        let (done, response) = oneshot::channel();
        std::thread::spawn(move || {
            let get = || -> anyhow::Result<Response<AsyncBody>> {
                let mut response = agent.get(&uri).call()?;
                let body = response.body_mut().read_to_vec()?;
                Ok(Response::builder()
                    .status(response.status())
                    .body(AsyncBody::from(body))?)
            };
            let _ = done.send(get());
        });
        response
            .map(|result| result.unwrap_or_else(|_| Err(anyhow::anyhow!("the request ended"))))
            .boxed()
    }
}
