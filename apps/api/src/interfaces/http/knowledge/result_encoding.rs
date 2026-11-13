use async_compression::tokio::bufread::GzipEncoder;
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, header},
    middleware::Next,
    response::Response,
};
use futures::TryStreamExt;
use tokio_util::io::{ReaderStream, StreamReader};

pub(super) async fn compress(request: Request, next: Next) -> Response {
    let gzip = accepts_gzip(request.headers());
    let mut response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    response.headers_mut().append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if !gzip {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    parts.headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    parts.headers.remove(header::CONTENT_LENGTH);
    let source = body.into_data_stream().map_err(std::io::Error::other);
    let encoder =
        GzipEncoder::with_quality(StreamReader::new(source), async_compression::Level::Fastest);
    Response::from_parts(parts, Body::from_stream(ReaderStream::new(encoder)))
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    let mut explicit = None::<f32>;
    let mut wildcard = 0.0_f32;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(value) = value.to_str() else { continue };
        for item in value.split(',') {
            let mut fields = item.split(';').map(str::trim);
            let Some(coding) = fields.next() else { continue };
            let mut quality = 1.0_f32;
            for field in fields {
                if let Some((name, value)) = field.split_once('=')
                    && name.trim().eq_ignore_ascii_case("q")
                {
                    quality = value
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|q| q.is_finite() && (0.0..=1.0).contains(q))
                        .unwrap_or(0.0);
                }
            }
            if coding.eq_ignore_ascii_case("gzip") {
                explicit = Some(explicit.map_or(quality, |q| q.max(quality)));
            } else if coding == "*" {
                wildcard = wildcard.max(quality);
            }
        }
    }
    explicit.unwrap_or(wildcard) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, routing::get};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;

    fn router() -> Router {
        let route = get(|| async {
            let chunks = (0..64)
                .map(|_| Ok::<_, std::io::Error>(Bytes::from_static(b"{\"s\":\"nodes\"}\n")))
                .chain(std::iter::once(Ok(Bytes::from_static(b"{\"s\":\"end\"}\n"))));
            Response::builder()
                .header(header::CONTENT_TYPE, "application/x-ndjson")
                .header(header::CONTENT_ENCODING, "identity")
                .body(Body::from_stream(futures::stream::iter(chunks)))
                .unwrap()
        })
        .layer(axum::middleware::from_fn(compress));
        crate::interfaces::http::middleware::compression::apply(
            Router::new().route("/result", route),
        )
    }

    #[test]
    fn negotiation_respects_explicit_rejection_and_wildcards() {
        for (value, expected) in [
            ("gzip", true),
            ("br, gzip;q=0.5", true),
            ("GZip; Q=1", true),
            ("gzip;q=0, *;q=1", false),
            ("*;q=0.5", true),
            ("gzip;q=NaN", false),
            ("gzip;q=2", false),
            ("br", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_str(value).unwrap());
            assert_eq!(accepts_gzip(&headers), expected, "{value}");
        }
        assert!(!accepts_gzip(&HeaderMap::new()));
    }

    #[tokio::test]
    async fn streaming_gzip_preserves_complete_ndjson_and_identity_fallback() {
        for encoding in ["gzip", "gzip;q=0", "identity"] {
            let response = router()
                .oneshot(
                    Request::builder()
                        .uri("/result")
                        .header(header::ACCEPT_ENCODING, encoding)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
            let compressed = response.headers()[header::CONTENT_ENCODING] == "gzip";
            assert_eq!(compressed, encoding == "gzip");
            let bytes = axum::body::to_bytes(response.into_body(), 65536).await.unwrap();
            let decoded = if compressed {
                let mut decoded = Vec::new();
                async_compression::tokio::bufread::GzipDecoder::new(&bytes[..])
                    .read_to_end(&mut decoded)
                    .await
                    .unwrap();
                assert!(bytes.len() < decoded.len());
                decoded
            } else {
                bytes.to_vec()
            };
            assert_eq!(
                decoded,
                [b"{\"s\":\"nodes\"}\n".repeat(64), b"{\"s\":\"end\"}\n".to_vec()].concat()
            );
        }
    }

    #[tokio::test]
    async fn failed_source_cannot_become_a_successful_gzip_body() {
        let route = get(|| async {
            Body::from_stream(futures::stream::iter([
                Ok(Bytes::from_static(b"{\"s\":\"nodes\"}\n")),
                Err(std::io::Error::other("artifact integrity failure")),
            ]))
        })
        .layer(axum::middleware::from_fn(compress));
        let response = Router::new()
            .route("/result", route)
            .oneshot(
                Request::builder()
                    .uri("/result")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(axum::body::to_bytes(response.into_body(), 65536).await.is_err());
    }

    #[tokio::test]
    async fn http1_gzip_chunk_framing_completes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, router())
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket.write_all(b"GET /result HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut bytes = Vec::new();
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(5), socket.read_to_end(&mut bytes))
                .await;
        let _ = stop.send(());
        server.await.unwrap().unwrap();
        read.unwrap().unwrap();
        let split = bytes.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
        let headers = std::str::from_utf8(&bytes[..split]).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-encoding: gzip\r\n"));
        assert!(headers.contains("transfer-encoding: chunked"));
        let mut remaining = &bytes[split + 4..];
        let mut compressed = Vec::new();
        loop {
            let end = remaining.windows(2).position(|v| v == b"\r\n").unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&remaining[..end]).unwrap(), 16).unwrap();
            remaining = &remaining[end + 2..];
            if size == 0 {
                assert_eq!(remaining, b"\r\n");
                break;
            }
            compressed.extend_from_slice(&remaining[..size]);
            assert_eq!(&remaining[size..size + 2], b"\r\n");
            remaining = &remaining[size + 2..];
        }
        let mut decoded = Vec::new();
        async_compression::tokio::bufread::GzipDecoder::new(&compressed[..])
            .read_to_end(&mut decoded)
            .await
            .unwrap();
        assert_eq!(
            decoded,
            [b"{\"s\":\"nodes\"}\n".repeat(64), b"{\"s\":\"end\"}\n".to_vec()].concat()
        );
    }
}
