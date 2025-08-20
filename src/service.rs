use crate::{Error, Result};
use http::{HeaderName, HeaderValue, Method, Request as KubeRequest, request::Builder as KubeBuilder};
use kube::{Client as KubeClient, Config as KubeConfig, config::KubeConfigOptions};
use reqwest::{Client as ReqwestClient, Request as ReqwestRequest, RequestBuilder as ReqwestBuilder};
use url::Url;

pub enum Builder {
    Kube(KubeBuilder),
    Reqwest(ReqwestBuilder),
}

impl Builder {
    pub fn header(self, key: HeaderName, value: HeaderValue) -> Self {
        match self {
            Self::Kube(b) => Builder::Kube(b.header(key, value)),
            Self::Reqwest(b) => Builder::Reqwest(b.header(key, value)),
        }
    }

    pub fn body<T>(self, body: T) -> Result<Request<T>>
    where
        T: Into<reqwest::Body>,
    {
        Ok(match self {
            Self::Kube(b) => Request::Kube(b.body(body).map_err(Error::HTTPError)?),
            Self::Reqwest(b) => Request::Reqwest({
                let b = b.body(body);
                b.build().map_err(Error::ReqwestError)?
            }),
        })
    }
}

pub enum Request<T> {
    Kube(KubeRequest<T>),
    Reqwest(ReqwestRequest),
}

#[derive(Clone)]
pub enum Client {
    Kube(KubeClient),
    Reqwest(ReqwestClient),
}

impl Client {
    pub fn new(client: KubeClient) -> Self {
        Self::Kube(client)
    }
    pub async fn try_default() -> Result<Self> {
        match KubeConfig::from_kubeconfig(&KubeConfigOptions::default()).await {
            Ok(config) => Ok(Self::Kube(
                KubeClient::try_from(config).map_err(Error::KubeError)?,
            )),
            Err(_) => Ok(Self::Reqwest(ReqwestClient::new())),
        }
    }

    pub fn post(&self, url: &str) -> Result<Builder> {
        Ok(match self {
            Self::Kube(_) => {
                let url = Url::parse(url).map_err(Error::URLParseError)?;
                let host = url
                    .host_str()
                    .ok_or_else(|| Error::ServiceRequestError(String::from("missing host in url")))?;
                let mut svc = None;
                let mut ns = None;
                let mut iter = host.split(".");
                (|iter| {
                    for p in iter {
                        match (svc, ns) {
                            (None, _) => svc = Some(p),
                            (_, None) => {
                                ns = Some(p);
                                return;
                            }
                            _ => panic!("should not be here"),
                        }
                    }
                })(&mut iter);
                match (svc, ns, url.port(), iter.count()) {
                    (Some(svc), Some(ns), Some(port), n) if n > 2 => {
                        Builder::Kube(KubeRequest::builder().method(Method::POST).uri(format!(
                            "/api/v1/namespaces/{}/services/{}:{}/proxy{}",
                            ns,
                            svc,
                            port,
                            url.path()
                        )))
                    }
                    _ => Err(Error::ServiceRequestError(
                        format!(
                            "{host} is not valid host, expected <svc>.<ns>.svc.<any-cluster-domain>:<port>"
                        )
                        .to_string(),
                    ))?,
                }
            }
            Self::Reqwest(client) => Builder::Reqwest(client.post(url)),
        })
    }

    pub async fn send(&self, request: Request<Vec<u8>>) -> Result<()> {
        match (self, request) {
            (Client::Kube(client), Request::Kube(request)) => {
                client.request_text(request).await.map_err(Error::KubeError)?;
            }
            (Client::Reqwest(client), Request::Reqwest(request)) => {
                client.execute(request).await.map_err(Error::ReqwestError)?;
            }
            _ => Err(Error::ServiceRequestError(String::from(
                "invalid request type for client",
            )))?,
        };
        Ok(())
    }
}
